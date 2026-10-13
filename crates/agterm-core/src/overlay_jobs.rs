//! The origin's private table of remote overlay jobs and the frames a job's helper connection carries (upstream's
//! `OverlayJobs.swift`).
//!
//! A job is an overlay the origin handed to a viewer presenting the session; a helper on the origin claims it with
//! `session.overlay.job.run`, receives the launch context, runs the program and reports how it ended. The table
//! settles the one race that matters, a claim against its launch deadline, with one winner, and keeps each job's first
//! terminal outcome so a late report can change nothing. Nothing here is a public command.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::OverlayPane;
use crate::overlay_capture::OverlayLaunchContext;
use crate::pane_env::uuid_string;

/// Open to claim: an ssh connect and authentication on the viewer, with room to spare.
pub const LAUNCH_WINDOW: Duration = Duration::from_secs(30);
/// Claim to `started`: the helper spawning a local child.
pub const START_WINDOW: Duration = Duration::from_secs(10);
/// Finished jobs kept for late lookups, oldest dropped first.
pub const FINISHED_RETENTION: usize = 32;
/// Past a deadline, so the expiry run at it sees the deadline passed (upstream's `seconds + 0.1`).
pub const EXPIRY_SLACK: Duration = Duration::from_millis(100);

/// How a remote overlay job ended (upstream's `OverlayJobOutcome`). `Unknown` means nobody can say: the helper never
/// reported starting, or went away without a terminal report. It never means the program exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayJobOutcome {
    Exited(i32),
    Canceled,
    LaunchFailed,
    Unknown,
}

impl OverlayJobOutcome {
    /// The name a caller reads for an outcome with no exit code.
    #[must_use]
    pub fn failure_name(self) -> Option<&'static str> {
        match self {
            Self::Exited(_) => None,
            Self::Canceled => Some("canceled"),
            Self::LaunchFailed => Some("launch-failed"),
            Self::Unknown => Some("unknown"),
        }
    }
}

/// Where a job is between its open and its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayJobState {
    /// Handed to the presenter, waiting for a helper to claim it before the deadline.
    Unclaimed {
        deadline: Instant,
    },
    /// A helper holds it and has the launch context, waiting for `started` before the deadline.
    Claimed {
        deadline: Instant,
    },
    Running,
    Finished(OverlayJobOutcome),
}

/// One overlay the origin handed to a presenter to show, run by a helper on the origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayJob {
    pub id: String,
    pub session: Uuid,
    /// The pane the overlay covers, `None` for the session-wide slot.
    pub pane: Option<OverlayPane>,
    /// The presenter generation the job was handed to.
    pub owner: u64,
    pub context: OverlayLaunchContext,
    pub state: OverlayJobState,
}

/// How the table reaches a claimed job's helper.
pub type CancelHook = Box<dyn FnMut()>;

/// The origin's table of remote overlay jobs (upstream's `OverlayJobs`).
pub struct OverlayJobs {
    now: Box<dyn Fn() -> Instant>,
    jobs: HashMap<String, OverlayJob>,
    cancel_hooks: HashMap<String, CancelHook>,
    finished_order: VecDeque<String>,
    /// Jobs that reached their first terminal outcome since the last [`OverlayJobs::take_finished`], in order.
    finished: Vec<OverlayJob>,
}

impl fmt::Debug for OverlayJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverlayJobs")
            .field("jobs", &self.jobs)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Default for OverlayJobs {
    fn default() -> Self {
        Self::new(Box::new(Instant::now))
    }
}

impl OverlayJobs {
    /// An empty table reading the time from `now`.
    #[must_use]
    pub fn new(now: Box<dyn Fn() -> Instant>) -> Self {
        Self {
            now,
            jobs: HashMap::new(),
            cancel_hooks: HashMap::new(),
            finished_order: VecDeque::new(),
            finished: Vec::new(),
        }
    }

    #[must_use]
    pub fn job(&self, id: &str) -> Option<&OverlayJob> {
        self.jobs.get(id)
    }

    /// Registers a job handed to a presenter; returns its id.
    pub fn register(
        &mut self,
        session: Uuid,
        pane: Option<OverlayPane>,
        owner: u64,
        context: OverlayLaunchContext,
    ) -> String {
        let id = uuid_string(Uuid::new_v4());
        let deadline = (self.now)() + LAUNCH_WINDOW;
        self.jobs.insert(
            id.clone(),
            OverlayJob {
                id: id.clone(),
                session,
                pane,
                owner,
                context,
                state: OverlayJobState::Unclaimed { deadline },
            },
        );
        id
    }

    /// A helper claims the job. Single use: the context is handed out once, only to a claim that beats the launch
    /// deadline. `cancel` is how the table reaches that helper later.
    pub fn claim(&mut self, id: &str, cancel: CancelHook) -> Option<OverlayLaunchContext> {
        let now = (self.now)();
        let claimable = self
            .jobs
            .get(id)
            .is_some_and(|job| matches!(job.state, OverlayJobState::Unclaimed { deadline } if now < deadline));
        if !claimable {
            self.expire();
            return None;
        }
        let job = self.jobs.get_mut(id)?;
        job.state = OverlayJobState::Claimed {
            deadline: now + START_WINDOW,
        };
        let context = job.context.clone();
        self.cancel_hooks.insert(id.to_owned(), cancel);
        Some(context)
    }

    /// The helper launched the program. Once running, no timeout can end the job; a start reported after the claim's
    /// deadline is too late and ends it `Unknown`, as the expiry would have.
    pub fn started(&mut self, id: &str) {
        let now = (self.now)();
        let Some(job) = self.jobs.get_mut(id) else { return };
        let OverlayJobState::Claimed { deadline } = job.state else {
            return;
        };
        if now < deadline {
            job.state = OverlayJobState::Running;
        } else {
            self.finish(id, OverlayJobOutcome::Unknown);
        }
    }

    /// Records a terminal outcome. Only the first counts; false for a later one or an unknown job.
    pub fn finish(&mut self, id: &str, outcome: OverlayJobOutcome) -> bool {
        let Some(job) = self.jobs.get_mut(id) else {
            return false;
        };
        if matches!(job.state, OverlayJobState::Finished(_)) {
            return false;
        }
        job.state = OverlayJobState::Finished(outcome);
        let finished = job.clone();
        self.cancel_hooks.remove(id);
        self.finished.push(finished);
        self.finished_order.push_back(id.to_owned());
        if self.finished_order.len() > FINISHED_RETENTION
            && let Some(oldest) = self.finished_order.pop_front()
        {
            self.jobs.remove(&oldest);
        }
        true
    }

    /// The helper's connection ended: without a terminal report first, nobody can say how the job ended.
    pub fn helper_gone(&mut self, id: &str) {
        self.finish(id, OverlayJobOutcome::Unknown);
    }

    /// Cancels a job: an unclaimed one ends `Canceled` here and a late claim is refused; a claimed or running one is
    /// cancelled through its helper, which reports the outcome. False when it is already over or unknown.
    pub fn cancel(&mut self, id: &str) -> bool {
        let Some(state) = self.jobs.get(id).map(|job| job.state) else {
            return false;
        };
        match state {
            OverlayJobState::Unclaimed { .. } => {
                self.finish(id, OverlayJobOutcome::Canceled);
            }
            OverlayJobState::Claimed { .. } | OverlayJobState::Running => {
                if let Some(hook) = self.cancel_hooks.get_mut(id) {
                    hook();
                }
            }
            OverlayJobState::Finished(_) => return false,
        }
        true
    }

    /// Ends every job whose deadline passed: an unclaimed one as `LaunchFailed`, a claimed one that never reported
    /// starting as `Unknown`. The owner calls this at each deadline it scheduled.
    pub fn expire(&mut self) {
        let now = (self.now)();
        let mut due: Vec<(String, OverlayJobOutcome)> = self
            .jobs
            .values()
            .filter_map(|job| match job.state {
                OverlayJobState::Unclaimed { deadline } if now >= deadline => {
                    Some((job.id.clone(), OverlayJobOutcome::LaunchFailed))
                }
                OverlayJobState::Claimed { deadline } if now >= deadline => {
                    Some((job.id.clone(), OverlayJobOutcome::Unknown))
                }
                _ => None,
            })
            .collect();
        due.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (id, outcome) in due {
            self.finish(&id, outcome);
        }
    }

    /// The jobs that reached their first terminal outcome since the last take, in order (upstream's `onFinished`).
    pub fn take_finished(&mut self) -> Vec<OverlayJob> {
        std::mem::take(&mut self.finished)
    }
}

/// One line on a helper's connection, after the ordinary reply to its claim (upstream's `OverlayJobFrame`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum OverlayJobFrame {
    /// Origin to helper, first: what to run.
    Context {
        context: OverlayLaunchContext,
    },
    /// Origin to helper: stop the program.
    Cancel,
    /// Helper to origin: the program is running.
    Started,
    Exited {
        code: i32,
    },
    Canceled,
    LaunchFailed {
        reason: String,
    },
}

impl OverlayJobFrame {
    /// One newline-terminated line.
    ///
    /// # Errors
    ///
    /// The frame could not be encoded.
    pub fn line(&self) -> serde_json::Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        Ok(line)
    }

    /// Decodes one line, its newline excluded.
    ///
    /// # Errors
    ///
    /// The line is not a frame.
    pub fn parse(line: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(line)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::pane_env::PaneEnv;

    /// A table over a clock the test moves.
    fn table() -> (OverlayJobs, Rc<Cell<Instant>>) {
        let clock = Rc::new(Cell::new(Instant::now()));
        let reading = Rc::clone(&clock);
        (OverlayJobs::new(Box::new(move || reading.get())), clock)
    }

    fn advance(clock: &Cell<Instant>, by: Duration) {
        clock.set(clock.get() + by);
    }

    fn context() -> OverlayLaunchContext {
        OverlayLaunchContext::new("true", "/", PaneEnv::new())
    }

    fn register(jobs: &mut OverlayJobs) -> String {
        jobs.register(Uuid::new_v4(), None, 1, context())
    }

    fn no_hook() -> CancelHook {
        Box::new(|| {})
    }

    #[test]
    fn a_claim_is_single_use() {
        let (mut jobs, _) = table();
        let id = register(&mut jobs);
        assert_eq!(jobs.claim(&id, no_hook()), Some(context()));
        assert_eq!(jobs.claim(&id, no_hook()), None, "the context is handed out once");
        assert_eq!(jobs.claim("nope", no_hook()), None);
    }

    #[test]
    fn a_claim_past_the_launch_deadline_loses_and_ends_the_job() {
        let (mut jobs, clock) = table();
        let id = register(&mut jobs);
        advance(&clock, LAUNCH_WINDOW);
        assert_eq!(jobs.claim(&id, no_hook()), None);
        assert_eq!(
            jobs.job(&id).map(|job| job.state),
            Some(OverlayJobState::Finished(OverlayJobOutcome::LaunchFailed))
        );
    }

    #[test]
    fn a_start_after_the_claim_deadline_ends_unknown() {
        let (mut jobs, clock) = table();
        let late = register(&mut jobs);
        jobs.claim(&late, no_hook());
        advance(&clock, START_WINDOW);
        jobs.started(&late);
        assert_eq!(
            jobs.job(&late).map(|job| job.state),
            Some(OverlayJobState::Finished(OverlayJobOutcome::Unknown))
        );
        let on_time = register(&mut jobs);
        jobs.claim(&on_time, no_hook());
        jobs.started(&on_time);
        advance(&clock, START_WINDOW * 10);
        jobs.expire();
        assert_eq!(jobs.job(&on_time).map(|job| job.state), Some(OverlayJobState::Running));
    }

    #[test]
    fn only_the_first_outcome_counts() {
        let (mut jobs, _) = table();
        let id = register(&mut jobs);
        jobs.claim(&id, no_hook());
        jobs.started(&id);
        assert!(jobs.finish(&id, OverlayJobOutcome::Exited(3)));
        assert!(!jobs.finish(&id, OverlayJobOutcome::Canceled));
        jobs.helper_gone(&id);
        let finished = jobs.take_finished();
        assert_eq!(finished.len(), 1);
        assert_eq!(
            finished[0].state,
            OverlayJobState::Finished(OverlayJobOutcome::Exited(3))
        );
    }

    #[test]
    fn cancel_ends_an_unclaimed_job_and_reaches_a_claimed_one_through_its_helper() {
        let (mut jobs, _) = table();
        let unclaimed = register(&mut jobs);
        assert!(jobs.cancel(&unclaimed));
        assert_eq!(
            jobs.job(&unclaimed).map(|job| job.state),
            Some(OverlayJobState::Finished(OverlayJobOutcome::Canceled))
        );
        assert_eq!(jobs.claim(&unclaimed, no_hook()), None, "a late claim is refused");
        assert!(!jobs.cancel(&unclaimed), "already over");

        let reached = Rc::new(RefCell::new(0));
        let counter = Rc::clone(&reached);
        let claimed = register(&mut jobs);
        jobs.claim(&claimed, Box::new(move || *counter.borrow_mut() += 1));
        assert!(jobs.cancel(&claimed));
        assert_eq!(*reached.borrow(), 1);
        assert!(matches!(
            jobs.job(&claimed).map(|job| job.state),
            Some(OverlayJobState::Claimed { .. })
        ));
        assert!(!jobs.cancel("nope"));
    }

    #[test]
    fn expiry_ends_unclaimed_and_unstarted_jobs() {
        let (mut jobs, clock) = table();
        let unclaimed = register(&mut jobs);
        let claimed = register(&mut jobs);
        jobs.claim(&claimed, no_hook());
        advance(&clock, LAUNCH_WINDOW);
        jobs.expire();
        let state = |id: &str| jobs.job(id).map(|job| job.state);
        assert_eq!(
            state(&unclaimed),
            Some(OverlayJobState::Finished(OverlayJobOutcome::LaunchFailed))
        );
        assert_eq!(
            state(&claimed),
            Some(OverlayJobState::Finished(OverlayJobOutcome::Unknown))
        );
    }

    #[test]
    fn finished_jobs_age_out_oldest_first() {
        let (mut jobs, _) = table();
        let ids: Vec<String> = (0..=FINISHED_RETENTION).map(|_| register(&mut jobs)).collect();
        for id in &ids {
            jobs.finish(id, OverlayJobOutcome::Canceled);
        }
        assert!(jobs.job(&ids[0]).is_none());
        assert!(jobs.job(&ids[1]).is_some());
    }

    #[test]
    fn frames_use_upstream_kinds() {
        let cases = [
            (OverlayJobFrame::Cancel, r#"{"kind":"cancel"}"#),
            (OverlayJobFrame::Started, r#"{"kind":"started"}"#),
            (OverlayJobFrame::Exited { code: 2 }, r#"{"kind":"exited","code":2}"#),
            (OverlayJobFrame::Canceled, r#"{"kind":"canceled"}"#),
            (
                OverlayJobFrame::LaunchFailed { reason: "no".into() },
                r#"{"kind":"launch-failed","reason":"no"}"#,
            ),
        ];
        for (frame, json) in cases {
            assert_eq!(frame.line().unwrap(), format!("{json}\n").into_bytes());
            assert_eq!(OverlayJobFrame::parse(json.as_bytes()).unwrap(), frame);
        }
        let context = OverlayJobFrame::Context { context: context() };
        let line = context.line().unwrap();
        assert_eq!(OverlayJobFrame::parse(&line[..line.len() - 1]).unwrap(), context);
        assert!(OverlayJobFrame::parse(br#"{"kind":"bogus"}"#).is_err());
    }

    #[test]
    fn only_exits_carry_no_failure_name() {
        assert_eq!(OverlayJobOutcome::Exited(0).failure_name(), None);
        assert_eq!(OverlayJobOutcome::LaunchFailed.failure_name(), Some("launch-failed"));
        assert_eq!(OverlayJobOutcome::Canceled.failure_name(), Some("canceled"));
        assert_eq!(OverlayJobOutcome::Unknown.failure_name(), Some("unknown"));
    }
}
