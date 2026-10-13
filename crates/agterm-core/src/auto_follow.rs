//! Auto-follow attention (upstream's `AppStore+AutoFollow.swift`): after the configured idle timeout a window jumps
//! its selection to the oldest blocked session, pulling the user to whatever agent is waiting.
//!
//! Only USER input counts as activity ([`Core::note_user_activity`]: a keystroke, a sidebar click); control commands
//! never do, or a script driving the app would keep the user "present". Each window debounces one fire: activity, an
//! enable or config change, and any change of the window's attention set (upstream observes `attentionSessions`; here
//! [`Core::take_auto_follow_timers`] compares a signature after every settle) push it out to a full timeout from now.
//! A fire with nothing to do does not re-arm: parked on a blocked session, the next keystroke or change re-arms.
//!
//! The frontend arms at most one main-loop timer per window: a later push only moves the deadline, and a timer that
//! fires early re-arms itself for the rest ([`AutoFollowTimer`]), so a typing burst does not pile up timers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agterm_proto::tree::ControlTree;
use uuid::Uuid;

use crate::Core;
use crate::model::{AgentIndicator, AgentStatus, AppStore, Session, StatusPane};

/// How early a timer may land and still count as due: the main loop rounds delays to milliseconds.
const FIRE_SLACK: Duration = Duration::from_millis(10);

/// The applied auto-follow settings (upstream's per-store `autoFollowTimeout` and `autoFollowStayOnActive`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowConfig {
    /// The idle grace before a fire.
    pub timeout: Duration,
    /// Never leave a current session whose agent is `active`.
    pub stay_on_active: bool,
}

/// One entry of a window's attention signature: a workspace (`None`) or a session with its indicator, in sidebar
/// order — what upstream's `attentionSessions` observation reads.
type AttentionEntry = (Uuid, Option<AgentIndicator>);

/// What the settings sync last applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsSync {
    /// Nothing yet: the first sync applies the settings whatever they say.
    #[default]
    Never,
    Applied(Option<FollowConfig>),
}

/// One window's auto-follow state.
#[derive(Debug, Default)]
struct WindowFollow {
    /// The last user activity, stamped whether or not auto-follow is on (the `tree.idleMs` metric).
    last_activity: Option<Instant>,
    /// When the armed fire is due; `None` while disarmed.
    due: Option<Instant>,
    /// When this window's live main-loop timer lands, if one is out.
    timer_at: Option<Instant>,
    /// Names the live timer: a timer of an older generation was superseded and does nothing.
    generation: u64,
    /// Open suppressors (an inline rename, a palette): a fire is a no-op while positive.
    suppression: usize,
    /// The attention set the last settle saw; `None` before the window was seen.
    attention: Option<Vec<AttentionEntry>>,
}

/// The auto-follow state of every window.
#[derive(Debug, Default)]
pub struct AutoFollow {
    /// `None` while auto-follow is off.
    config: Option<FollowConfig>,
    /// The settings-derived config the last sync applied.
    synced: SettingsSync,
    windows: HashMap<Uuid, WindowFollow>,
    /// Timers armed since the frontend's last take.
    pending: Vec<AutoFollowTimer>,
}

/// A debounced auto-follow fire the frontend arms on the main loop: after `delay`, hand it back to
/// [`Core::fire_auto_follow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoFollowTimer {
    pub window: Uuid,
    pub generation: u64,
    pub delay: Duration,
}

/// An auto-follow moved window `window`'s selection to `session` (upstream's `.agtermAutoFollowed`). `indicator` is
/// the destination's indicator from before the selection reset it, which names the pane to reveal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoFollowed {
    pub window: Uuid,
    pub session: Uuid,
    pub indicator: Option<AgentIndicator>,
}

impl AutoFollow {
    /// Pushes window `window`'s fire out to `delay` from `now` (upstream's `armAutoFollow`, a debouncer reschedule).
    fn arm(&mut self, window: Uuid, delay: Duration, now: Instant) {
        let state = self.windows.entry(window).or_default();
        let due = now + delay;
        state.due = Some(due);
        // a live timer landing no later than the deadline re-arms itself for the rest when it fires
        if state.timer_at.is_some_and(|at| at <= due) {
            return;
        }
        self.queue(window, due, delay);
    }

    /// Hands the frontend a new timer for `window`, superseding the live one.
    fn queue(&mut self, window: Uuid, due: Instant, delay: Duration) {
        let Some(state) = self.windows.get_mut(&window) else {
            return;
        };
        state.generation += 1;
        state.timer_at = Some(due);
        let generation = state.generation;
        self.pending.retain(|timer| timer.window != window);
        self.pending.push(AutoFollowTimer {
            window,
            generation,
            delay,
        });
    }

    /// Drops window `window`'s pending fire (upstream's `autoFollowDebouncer.cancel`).
    fn cancel(&mut self, window: Uuid) {
        if let Some(state) = self.windows.get_mut(&window) {
            state.due = None;
        }
    }

    /// Whether `timer` landing at `now` is a due fire; an early one re-arms for the rest of the deadline.
    fn due(&mut self, timer: AutoFollowTimer, now: Instant) -> bool {
        let Some(state) = self.windows.get_mut(&timer.window) else {
            return false;
        };
        if state.generation != timer.generation {
            return false;
        }
        state.timer_at = None;
        let Some(due) = state.due else {
            return false;
        };
        if due > now + FIRE_SLACK {
            self.queue(timer.window, due, due - now);
            return false;
        }
        state.due = None;
        true
    }

    fn suppressed(&self, window: Uuid) -> bool {
        self.windows.get(&window).is_some_and(|state| state.suppression > 0)
    }

    fn stay_on_active(&self) -> bool {
        self.config.is_some_and(|config| config.stay_on_active)
    }
}

impl Core {
    /// User input in session `session`'s pane (a physical keystroke) or a manual selection of it (upstream's
    /// `noteUserActivity`): stamps its window's activity whether or not auto-follow is on, then re-arms the window's
    /// fire, or drops it while auto-follow is off. A session in no window (the quick terminal's) counts for the
    /// frontmost window, as upstream's `activeStore`. Only USER entry points call it: auto-follow's own selection must
    /// not reset the idle clock. Arm what [`Core::take_auto_follow_timers`] then hands back.
    pub fn note_user_activity(&mut self, session: Uuid) {
        let window = self
            .library()
            .window_id_for_session(session)
            .or_else(|| self.library().active_window_id());
        if let Some(window) = window {
            self.note_window_activity(window);
        }
    }

    /// [`Core::note_user_activity`] for window `window` itself.
    pub fn note_window_activity(&mut self, window: Uuid) {
        let now = Instant::now();
        self.auto_follow.windows.entry(window).or_default().last_activity = Some(now);
        match self.auto_follow.config {
            Some(config) => self.auto_follow.arm(window, config.timeout, now),
            None => self.auto_follow.cancel(window),
        }
    }

    /// A non-terminal editor or overlay took the keyboard in window `window` (an inline rename, a palette): its fires
    /// are no-ops until the matching [`Core::resume_auto_follow`]. Counted, so overlapping suppressors hold to the last.
    pub fn suppress_auto_follow(&mut self, window: Uuid) {
        self.auto_follow.windows.entry(window).or_default().suppression += 1;
    }

    /// Lifts one suppression of window `window`, clamped at zero. Does not re-arm: the next keystroke or change does.
    pub fn resume_auto_follow(&mut self, window: Uuid) {
        if let Some(state) = self.auto_follow.windows.get_mut(&window) {
            state.suppression = state.suppression.saturating_sub(1);
        }
    }

    /// The applied auto-follow config; `None` while off.
    #[must_use]
    pub fn auto_follow_config(&self) -> Option<FollowConfig> {
        self.auto_follow.config
    }

    /// Enables (`Some`) or disables auto-follow in every window (upstream's `setAutoFollow`, fanned out by the
    /// settings): off drops every pending fire; on, or a changed timeout or stay-on-active, arms every open window
    /// from its current state, so a user already idle with a block waiting is pulled after the grace.
    pub fn set_auto_follow(&mut self, config: Option<FollowConfig>) {
        let previous = std::mem::replace(&mut self.auto_follow.config, config);
        let windows = self.library().open_ids();
        match config {
            None => windows.into_iter().for_each(|window| self.auto_follow.cancel(window)),
            Some(config) if previous != Some(config) => {
                let now = Instant::now();
                for window in windows {
                    self.auto_follow.arm(window, config.timeout, now);
                }
            }
            Some(_) => {}
        }
    }

    /// The auto-follow fires armed since the last take, for the frontend to schedule. Run after every settle: it
    /// applies changed settings and re-arms each window whose attention set changed while auto-follow is on (a block
    /// that lands, or an agent that finishes, while the user is already idle gets the same grace a keystroke buys).
    pub fn take_auto_follow_timers(&mut self) -> Vec<AutoFollowTimer> {
        self.sync_auto_follow_settings();
        self.rearm_on_attention_change(Instant::now());
        std::mem::take(&mut self.auto_follow.pending)
    }

    /// Applies the settings' auto-follow when it differs from what the last sync applied, so a later
    /// [`Core::set_auto_follow`] stands until the settings change again.
    fn sync_auto_follow_settings(&mut self) {
        let settings = self.settings();
        let config = settings
            .effective_auto_follow_attention()
            .timeout_secs()
            .map(|secs| FollowConfig {
                timeout: Duration::from_secs(u64::from(secs)),
                stay_on_active: settings.auto_follow_stay_on_active.unwrap_or(false),
            });
        if self.auto_follow.synced != SettingsSync::Applied(config) {
            self.auto_follow.synced = SettingsSync::Applied(config);
            self.set_auto_follow(config);
        }
    }

    fn rearm_on_attention_change(&mut self, now: Instant) {
        let open = self.library().open_ids();
        self.auto_follow.windows.retain(|window, _| open.contains(window));
        for window in open {
            let Some(signature) = self.library().store(window).map(attention_signature) else {
                continue;
            };
            let state = self.auto_follow.windows.entry(window).or_default();
            if state.attention.as_ref() == Some(&signature) {
                continue;
            }
            state.attention = Some(signature);
            if let Some(config) = self.auto_follow.config {
                self.auto_follow.arm(window, config.timeout, now);
            }
        }
    }

    /// An armed auto-follow delay elapsed: fire unless a later arm superseded it or pushed it out (then the next timer
    /// is armed). What the fire selected, for the frontend's focus. Commit afterwards.
    pub fn fire_auto_follow(&mut self, timer: AutoFollowTimer) -> Option<AutoFollowed> {
        self.fire_auto_follow_at(timer, Instant::now())
    }

    pub(crate) fn fire_auto_follow_at(&mut self, timer: AutoFollowTimer, now: Instant) -> Option<AutoFollowed> {
        if !self.auto_follow.due(timer, now) {
            return None;
        }
        self.auto_follow_step(timer.window)
    }

    /// One auto-follow step in window `window` (upstream's `autoFollowFire`): unless suppressed, selects the
    /// [`auto_follow_target`] among the window's blocked sessions, without noting activity, and mutes it until it
    /// re-enters blocked. No target is a no-op and does not re-arm.
    pub(crate) fn auto_follow_step(&mut self, window: Uuid) -> Option<AutoFollowed> {
        // an open dashboard suppresses as upstream's dashboard open does (`suppressAutoFollow` until it closes), so
        // an armed jump cannot reshuffle the selection under the grid
        if self.auto_follow.suppressed(window) || self.dashboard(window).is_some() {
            return None;
        }
        let stay_on_active = self.auto_follow.stay_on_active();
        let store = self.library_mut().store_mut(window)?;
        let blocked: Vec<&Session> = store
            .attention_sessions()
            .into_iter()
            .filter(|session| session.agent_indicator.status == AgentStatus::Blocked)
            .collect();
        let target = auto_follow_target(store.active_session(), &blocked, stay_on_active)?;
        let indicator = store.select_session(Some(target));
        if let Some(session) = store.session_mut(target) {
            session.set_auto_follow_consumed(true);
        }
        Some(AutoFollowed {
            window,
            session: target,
            indicator,
        })
    }

    /// Lands keyboard focus on the pane that set selected session `session`'s status, from `indicator` captured
    /// before the selection reset it (upstream's `revealActiveBlockedPane`): a blocked or completed status hides a
    /// scratch that masks another pane, focuses a split pane that tagged it, or shows the scratch that did; anything
    /// else just focuses the session. Commit afterwards.
    pub fn reveal_blocked_pane(&mut self, session: Uuid, indicator: Option<&AgentIndicator>) {
        self.request_pane_focus(session);
        let Some(indicator) = indicator.filter(|indicator| indicator.status.needs_attention()) else {
            return;
        };
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return;
        };
        reveal_pane(store, session, indicator.status_pane);
    }

    /// [`Core::reveal_blocked_pane`] in window `window`'s copy of `session`, for a caller that already resolved the
    /// window: a legacy snapshot may hold one session id in two open windows.
    pub(crate) fn reveal_blocked_pane_in(&mut self, window: Uuid, session: Uuid, indicator: Option<&AgentIndicator>) {
        self.request_pane_focus(session);
        let Some(indicator) = indicator.filter(|indicator| indicator.status.needs_attention()) else {
            return;
        };
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        reveal_pane(store, session, indicator.status_pane);
    }
}

fn reveal_pane(store: &mut AppStore, id: Uuid, pane: Option<StatusPane>) {
    let Some(current) = store.session(id) else {
        return;
    };
    let (scratch_shown, has_split_surface) = (current.scratch_active, current.split_surface.is_present());
    if pane != Some(StatusPane::Scratch) && scratch_shown {
        store.toggle_scratch(id, true);
    }
    match pane {
        Some(StatusPane::Scratch) => {
            if !scratch_shown {
                store.toggle_scratch(id, true);
            }
        }
        Some(StatusPane::Right) if has_split_surface => set_split_focused(store, id, true),
        _ => set_split_focused(store, id, false),
    }
}

fn set_split_focused(store: &mut AppStore, id: Uuid, focused: bool) {
    if let Some(session) = store.session_mut(id) {
        session.split.focused = focused;
    }
}

/// The pure auto-follow decision (upstream's `autoFollowTarget`): stay put when the current session is blocked
/// (you're on it) or, under `stay_on_active`, active (don't leave a running agent); else the oldest of `blocked` not
/// already followed, by status change time, an unstamped one last.
#[must_use]
pub fn auto_follow_target(current: Option<&Session>, blocked: &[&Session], stay_on_active: bool) -> Option<Uuid> {
    let current_status = current.map(|session| session.agent_indicator.status);
    if current_status == Some(AgentStatus::Blocked) || (stay_on_active && current_status == Some(AgentStatus::Active)) {
        return None;
    }
    blocked
        .iter()
        .filter(|session| !session.auto_follow_consumed())
        .min_by_key(|session| (session.status_changed_at.is_none(), session.status_changed_at))
        .map(|session| session.id())
}

/// What upstream's attention observation reads: the workspaces, their sessions and each session's indicator.
fn attention_signature(store: &AppStore) -> Vec<AttentionEntry> {
    let mut signature = Vec::new();
    for workspace in store.workspaces() {
        signature.push((workspace.id(), None));
        for session in &workspace.sessions {
            signature.push((session.id(), Some(session.agent_indicator.clone())));
        }
    }
    signature
}

/// Milliseconds from `last` to `now`, clamped at zero (upstream's `idleMs(asOf:)`).
#[must_use]
pub fn idle_ms(last: Instant, now: Instant) -> i64 {
    i64::try_from(now.saturating_duration_since(last).as_millis()).unwrap_or(i64::MAX)
}

/// Fills the tree's `idleMs` for window `window`: the time since its last user activity, absent before any.
pub(crate) fn project(core: &Core, window: Option<Uuid>, tree: &mut ControlTree) {
    tree.idle_ms = window
        .and_then(|window| core.auto_follow.windows.get(&window)?.last_activity)
        .map(|last| idle_ms(last, Instant::now()));
}
