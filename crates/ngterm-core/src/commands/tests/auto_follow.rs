//! Auto-follow attention (upstream's `AppStoreAutoFollowTests`).

use std::time::{Duration, Instant, SystemTime};

use ngterm_host::fakes::FakeSurface;
use ngterm_proto::command::SessionCmd;
use ngterm_proto::{Args, Command};

use super::{Fixture, args, wire};
use crate::Core;
use crate::auto_follow::{AutoFollowTimer, AutoFollowed, FollowConfig, auto_follow_target, idle_ms};
use crate::model::{AgentIndicator, AgentStatus, Session, StatusPane};
use crate::persist::AppSettings;
use uuid::Uuid;

const GRACE: Duration = Duration::from_secs(100);

fn indicator(status: AgentStatus) -> AgentIndicator {
    AgentIndicator {
        status,
        ..AgentIndicator::default()
    }
}

fn set_status(fixture: &mut Fixture, id: Uuid, indicator: AgentIndicator) {
    fixture.store_mut().unwrap().set_agent_indicator(indicator, id);
}

/// Marks `id` blocked and pins its change time to `at` seconds past the epoch, so the FIFO order is deterministic.
fn block_at(fixture: &mut Fixture, id: Uuid, at: u64) {
    set_status(fixture, id, indicator(AgentStatus::Blocked));
    stamp(fixture, id, Some(SystemTime::UNIX_EPOCH + Duration::from_secs(at)));
}

fn stamp(fixture: &mut Fixture, id: Uuid, at: Option<SystemTime>) {
    fixture.store_mut().unwrap().session_mut(id).unwrap().status_changed_at = at;
}

/// A new session in the first workspace, blocked at `at`.
fn add_blocked(fixture: &mut Fixture, at: u64) -> Uuid {
    let id = fixture.add_session(fixture.workspace);
    block_at(fixture, id, at);
    id
}

fn select(fixture: &mut Fixture, id: Uuid) {
    fixture.store_mut().unwrap().select_session(Some(id));
}

fn session(fixture: &Fixture, id: Uuid) -> &Session {
    fixture.store().session(id).unwrap()
}

fn consumed(fixture: &Fixture, id: Uuid) -> bool {
    session(fixture, id).auto_follow_consumed()
}

/// One fire in the fixture's window, as upstream's tests call `autoFollowFire` directly.
fn fire(fixture: &mut Fixture) -> Option<AutoFollowed> {
    let window = fixture.window;
    fixture.core.auto_follow_step(window)
}

fn target(fixture: &Fixture, current: Option<Uuid>, blocked: &[Uuid], stay_on_active: bool) -> Option<Uuid> {
    let current = current.map(|id| session(fixture, id));
    let blocked: Vec<&Session> = blocked.iter().map(|id| session(fixture, *id)).collect();
    auto_follow_target(current, &blocked, stay_on_active)
}

fn config(stay_on_active: bool) -> FollowConfig {
    FollowConfig {
        timeout: GRACE,
        stay_on_active,
    }
}

/// Turns auto-follow on as the settings fan-out does; the first take applies the (off) settings first, so the
/// explicit config stands from then on.
fn enable(fixture: &mut Fixture, stay_on_active: bool) {
    let _ = fixture.core.take_auto_follow_timers();
    fixture.core.set_auto_follow(Some(config(stay_on_active)));
}

/// Runs every armed fire once its delay passed (upstream's `autoFollowDebouncer.flush()`), the settle's re-arm
/// included.
fn flush(core: &mut Core) -> Vec<AutoFollowed> {
    let timers = core.take_auto_follow_timers();
    let now = Instant::now() + timers.iter().map(|timer| timer.delay).max().unwrap_or_default();
    timers
        .into_iter()
        .filter_map(|timer| core.fire_auto_follow_at(timer, now))
        .collect()
}

fn idle(fixture: &Fixture) -> Option<i64> {
    fixture.core.tree(None).unwrap().idle_ms
}

// MARK: - auto_follow_target (pure decision)

#[test]
fn target_picks_oldest_blocked() {
    let mut fixture = Fixture::new(0);
    let older = add_blocked(&mut fixture, 100);
    let newer = add_blocked(&mut fixture, 200);
    assert_eq!(target(&fixture, None, &[newer, older], false), Some(older));
}

#[test]
fn target_suppresses_when_current_blocked() {
    let mut fixture = Fixture::new(0);
    let current = add_blocked(&mut fixture, 50);
    let other = add_blocked(&mut fixture, 200);
    assert_eq!(target(&fixture, Some(current), &[current, other], false), None);
    assert_eq!(target(&fixture, Some(current), &[current, other], true), None);
}

#[test]
fn target_suppresses_active_only_when_stay_on_active() {
    let mut fixture = Fixture::new(0);
    let current = fixture.add_session(fixture.workspace);
    set_status(&mut fixture, current, indicator(AgentStatus::Active));
    let blocked = add_blocked(&mut fixture, 100);
    assert_eq!(target(&fixture, Some(current), &[blocked], true), None);
    assert_eq!(target(&fixture, Some(current), &[blocked], false), Some(blocked));
}

#[test]
fn target_of_no_blocked_is_none() {
    let fixture = Fixture::new(0);
    assert_eq!(target(&fixture, None, &[], false), None);
    assert_eq!(target(&fixture, Some(fixture.sessions[0]), &[], false), None);
}

#[test]
fn target_sorts_a_missing_stamp_last() {
    let mut fixture = Fixture::new(0);
    let stamped = add_blocked(&mut fixture, 500);
    let unstamped = add_blocked(&mut fixture, 1);
    stamp(&mut fixture, unstamped, None);
    assert_eq!(target(&fixture, None, &[unstamped, stamped], false), Some(stamped));
}

#[test]
fn target_skips_consumed_blocked() {
    let mut fixture = Fixture::new(0);
    let older = add_blocked(&mut fixture, 100);
    let newer = add_blocked(&mut fixture, 200);
    fixture
        .store_mut()
        .unwrap()
        .session_mut(older)
        .unwrap()
        .set_auto_follow_consumed(true);
    assert_eq!(target(&fixture, None, &[older, newer], false), Some(newer));
    fixture
        .store_mut()
        .unwrap()
        .session_mut(newer)
        .unwrap()
        .set_auto_follow_consumed(true);
    assert_eq!(target(&fixture, None, &[older, newer], false), None);
}

// MARK: - the fire (window-wide filter + select)

#[test]
fn fire_selects_oldest_blocked_across_workspaces() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let (_, away) = fixture.add_workspace("away", 1);
    block_at(&mut fixture, away[0], 100);
    add_blocked(&mut fixture, 200);
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(away[0]));
}

#[test]
fn fire_is_suppressed_when_parked_on_blocked() {
    let mut fixture = Fixture::new(0);
    let current = add_blocked(&mut fixture, 50);
    add_blocked(&mut fixture, 200);
    select(&mut fixture, current);
    assert_eq!(fire(&mut fixture), None);
    assert_eq!(fixture.selected(), Some(current));
}

#[test]
fn fire_is_a_no_op_without_blocked() {
    let mut fixture = Fixture::new(1);
    let (a, b) = (fixture.sessions[0], fixture.sessions[1]);
    set_status(&mut fixture, b, indicator(AgentStatus::Active));
    select(&mut fixture, a);
    assert_eq!(fire(&mut fixture), None);
    assert_eq!(fixture.selected(), Some(a));
}

#[test]
fn fire_advances_after_the_current_clears() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let older = add_blocked(&mut fixture, 100);
    let newer = add_blocked(&mut fixture, 200);
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(older));
    set_status(&mut fixture, older, AgentIndicator::default());
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(newer));
}

#[test]
fn fire_does_not_note_activity() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    assert!(fire(&mut fixture).is_some());
    assert_eq!(idle(&fixture), None);
}

#[test]
fn fire_reports_the_pre_selection_indicator() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let blocked = fixture.add_session(fixture.workspace);
    select(&mut fixture, idle_session);
    fixture.store_mut().unwrap().session_mut(blocked).unwrap().split.exists = true;
    let expected = AgentIndicator {
        status: AgentStatus::Blocked,
        auto_reset: true,
        status_pane: Some(StatusPane::Right),
        ..AgentIndicator::default()
    };
    set_status(&mut fixture, blocked, expected.clone());
    let followed = fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(blocked));
    assert_eq!(session(&fixture, blocked).agent_indicator, AgentIndicator::default());
    assert_eq!(
        followed,
        Some(AutoFollowed {
            window: fixture.window,
            session: blocked,
            indicator: Some(expected),
        })
    );
}

#[test]
fn fire_self_trigger_terminates() {
    let mut fixture = Fixture::new(0);
    let completed = fixture.sessions[0];
    let blocked = add_blocked(&mut fixture, 100);
    select(&mut fixture, completed);
    let one_time = AgentIndicator {
        auto_reset: true,
        ..indicator(AgentStatus::Completed)
    };
    set_status(&mut fixture, completed, one_time);
    enable(&mut fixture, false);
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(blocked));
    assert_eq!(session(&fixture, completed).agent_indicator.status, AgentStatus::Idle);
    // the reset glyph changed the attention set, which re-arms once; parked on the block, that fire stops it
    assert!(flush(&mut fixture.core).is_empty());
    assert_eq!(fixture.selected(), Some(blocked));
    assert!(fixture.core.take_auto_follow_timers().is_empty());
}

// MARK: - muting a block already followed

#[test]
fn fire_marks_consumed_and_does_not_return() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let x = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(x));
    assert!(consumed(&fixture, x));
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(idle_session));
}

#[test]
fn fire_walks_each_block_once_then_stops() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let x = add_blocked(&mut fixture, 100);
    let z = add_blocked(&mut fixture, 200);
    for expected in [x, z, idle_session] {
        select(&mut fixture, idle_session);
        fire(&mut fixture);
        assert_eq!(fixture.selected(), Some(expected));
    }
}

#[test]
fn consumed_survives_a_blocked_reassert() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let x = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    assert!(consumed(&fixture, x));
    select(&mut fixture, idle_session);
    set_status(&mut fixture, x, indicator(AgentStatus::Blocked));
    assert!(consumed(&fixture, x));
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(idle_session));
}

#[test]
fn fire_returns_after_the_block_reenters() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let x = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    select(&mut fixture, idle_session);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(idle_session));
    set_status(&mut fixture, x, indicator(AgentStatus::Active));
    set_status(&mut fixture, x, indicator(AgentStatus::Blocked));
    assert!(!consumed(&fixture, x));
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(x));
}

// MARK: - suppression (a non-terminal editor or palette owns the keyboard)

#[test]
fn fire_is_suppressed_while_an_editor_is_active() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let blocked = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    fixture.core.suppress_auto_follow(fixture.window);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(idle_session));
    fixture.core.resume_auto_follow(fixture.window);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(blocked));
}

#[test]
fn suppression_nests_and_clamps_at_zero() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let blocked = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    let window = fixture.window;
    fixture.core.suppress_auto_follow(window);
    fixture.core.suppress_auto_follow(window);
    fixture.core.resume_auto_follow(window);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(idle_session));
    fixture.core.resume_auto_follow(window);
    fixture.core.resume_auto_follow(window);
    fire(&mut fixture);
    assert_eq!(fixture.selected(), Some(blocked));
}

// MARK: - user activity and idleMs

#[test]
fn control_input_never_notes_activity() {
    let mut fixture = Fixture::new(1);
    let (a, b) = (fixture.sessions[0], fixture.sessions[1]);
    fixture.session(SessionCmd::Select, Some(&wire(a)), Args::default());
    fixture.session(SessionCmd::Go, None, args(|args| args.to = Some("next".into())));
    let status = args(|args| args.status = Some("blocked".into()));
    fixture.session(SessionCmd::Status, Some(&wire(b)), status);
    fixture.send(Command::Tree, None, Args::default());
    assert_eq!(idle(&fixture), None);
    fixture.core.note_user_activity(a);
    select(&mut fixture, b);
    assert!(idle(&fixture).is_some_and(|ms| ms >= 0));
}

#[test]
fn user_activity_stamps_only_its_window() {
    let mut fixture = Fixture::new(0);
    let other = fixture.core.library_mut().new_window(None);
    fixture.core.note_user_activity(fixture.sessions[0]);
    let idle_of = |window: Uuid| fixture.core.tree(Some(&wire(window))).unwrap().idle_ms;
    assert!(idle_of(fixture.window).is_some());
    assert_eq!(idle_of(other), None);
}

#[test]
fn idle_ms_measures_from_the_last_activity() {
    let last = Instant::now();
    assert_eq!(idle_ms(last, last + Duration::from_millis(2500)), 2500);
    assert_eq!(idle_ms(last, last), 0);
    // a `now` before the activity clamps to zero rather than going negative
    assert_eq!(idle_ms(last + Duration::from_secs(1), last), 0);
}

#[test]
fn tree_projects_idle_and_auto_follow_ms() {
    let settings = AppSettings {
        auto_follow_attention: Some("s30".to_owned()),
        ..AppSettings::default()
    };
    let mut fixture = Fixture::with_settings(0, settings);
    let tree = fixture.core.tree(None).unwrap();
    assert_eq!((tree.idle_ms, tree.auto_follow_ms), (None, Some(30_000)));
    fixture.core.note_window_activity(fixture.window);
    assert!(idle(&fixture).is_some_and(|ms| ms >= 0));
}

// MARK: - arming: activity, settings, attention changes

/// A fixture parked on an idle session with one blocked session waiting; returns (idle, blocked).
fn parked() -> (Fixture, Uuid, Uuid) {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let blocked = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    (fixture, idle_session, blocked)
}

#[test]
fn activity_with_a_timeout_schedules_a_fire() {
    let (mut fixture, idle_session, blocked) = parked();
    enable(&mut fixture, false);
    fixture.core.note_user_activity(idle_session);
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(blocked));
}

#[test]
fn activity_without_a_timeout_cancels_the_fire() {
    let (mut fixture, idle_session, _) = parked();
    enable(&mut fixture, false);
    fixture.core.note_user_activity(idle_session);
    fixture.core.set_auto_follow(None);
    fixture.core.note_user_activity(idle_session);
    assert!(flush(&mut fixture.core).is_empty());
    assert_eq!(fixture.selected(), Some(idle_session));
}

#[test]
fn enabling_arms_from_the_current_state() {
    let (mut fixture, _, blocked) = parked();
    enable(&mut fixture, false);
    assert_eq!(fixture.core.auto_follow_config(), Some(config(false)));
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(blocked));
}

#[test]
fn enabling_with_stay_on_active_keeps_an_active_current() {
    let mut fixture = Fixture::new(0);
    let active = fixture.sessions[0];
    set_status(&mut fixture, active, indicator(AgentStatus::Active));
    add_blocked(&mut fixture, 100);
    select(&mut fixture, active);
    enable(&mut fixture, true);
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(active));
}

#[test]
fn disabling_cancels_the_pending_fire() {
    let (mut fixture, idle_session, _) = parked();
    enable(&mut fixture, false);
    fixture.core.set_auto_follow(None);
    assert_eq!(fixture.core.auto_follow_config(), None);
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(idle_session));
}

#[test]
fn a_status_change_while_enabled_arms() {
    let mut fixture = Fixture::new(1);
    let (idle_session, s) = (fixture.sessions[0], fixture.sessions[1]);
    select(&mut fixture, idle_session);
    enable(&mut fixture, false);
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(idle_session));
    set_status(&mut fixture, s, indicator(AgentStatus::Blocked));
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(s));
}

#[test]
fn a_status_change_while_disabled_does_not_arm() {
    let mut fixture = Fixture::new(1);
    let (idle_session, s) = (fixture.sessions[0], fixture.sessions[1]);
    select(&mut fixture, idle_session);
    enable(&mut fixture, false);
    flush(&mut fixture.core);
    fixture.core.set_auto_follow(None);
    set_status(&mut fixture, s, indicator(AgentStatus::Blocked));
    assert!(fixture.core.take_auto_follow_timers().is_empty());
    assert_eq!(fixture.selected(), Some(idle_session));
}

#[test]
fn a_timeout_change_while_enabled_rearms() {
    let (mut fixture, _, blocked) = parked();
    enable(&mut fixture, false);
    let shorter = FollowConfig {
        timeout: Duration::from_secs(30),
        stay_on_active: false,
    };
    fixture.core.set_auto_follow(Some(shorter));
    let timers = fixture.core.take_auto_follow_timers();
    let delays: Vec<Duration> = timers.iter().map(|timer| timer.delay).collect();
    assert_eq!(delays, [shorter.timeout]);
    let now = Instant::now() + shorter.timeout;
    for timer in timers {
        let _ = fixture.core.fire_auto_follow_at(timer, now);
    }
    assert_eq!(fixture.selected(), Some(blocked));
}

#[test]
fn a_stay_on_active_change_while_enabled_rearms() {
    let mut fixture = Fixture::new(0);
    let active = fixture.sessions[0];
    set_status(&mut fixture, active, indicator(AgentStatus::Active));
    let blocked = add_blocked(&mut fixture, 100);
    select(&mut fixture, active);
    enable(&mut fixture, true);
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(active));
    fixture.core.set_auto_follow(Some(config(false)));
    flush(&mut fixture.core);
    assert_eq!(fixture.selected(), Some(blocked));
}

#[test]
fn enabled_settings_arm_every_open_window_at_startup() {
    let settings = AppSettings {
        auto_follow_attention: Some("s5".to_owned()),
        auto_follow_stay_on_active: Some(true),
        ..AppSettings::default()
    };
    let mut fixture = Fixture::with_settings(0, settings);
    let other = fixture.core.library_mut().new_window(None);
    let timers = fixture.core.take_auto_follow_timers();
    let mut windows: Vec<Uuid> = timers.iter().map(|timer| timer.window).collect();
    windows.sort();
    let mut expected = vec![fixture.window, other];
    expected.sort();
    assert_eq!(windows, expected);
    assert!(timers.iter().all(|timer| timer.delay == Duration::from_secs(5)));
    let applied = fixture.core.auto_follow_config().map(|config| config.stay_on_active);
    assert_eq!(applied, Some(true));
    assert!(fixture.core.take_auto_follow_timers().is_empty());
}

// MARK: - the debounce: one live timer per window

#[test]
fn a_typing_burst_arms_one_timer_and_an_early_fire_rearms_for_the_rest() {
    let (mut fixture, idle_session, blocked) = parked();
    enable(&mut fixture, false);
    let first = fixture.core.take_auto_follow_timers();
    assert_eq!(first.len(), 1);
    fixture.core.note_user_activity(idle_session);
    fixture.core.note_user_activity(idle_session);
    assert!(fixture.core.take_auto_follow_timers().is_empty());
    // landing before the pushed-out deadline hands back a timer for the rest
    let landed = Instant::now() + GRACE.saturating_sub(Duration::from_secs(1));
    assert_eq!(fixture.core.fire_auto_follow_at(first[0], landed), None);
    assert_eq!(fixture.selected(), Some(idle_session));
    let rest = fixture.core.take_auto_follow_timers();
    assert_eq!(rest.len(), 1);
    assert!(rest[0].delay > Duration::ZERO && rest[0].delay <= Duration::from_secs(1));
    assert_eq!(
        fixture.core.fire_auto_follow_at(first[0], Instant::now() + GRACE * 2),
        None
    );
    assert!(
        fixture
            .core
            .fire_auto_follow_at(rest[0], Instant::now() + GRACE)
            .is_some()
    );
    assert_eq!(fixture.selected(), Some(blocked));
}

#[test]
fn a_timer_of_an_unknown_window_does_nothing() {
    let (mut fixture, idle_session, _) = parked();
    let timer = AutoFollowTimer {
        window: Uuid::new_v4(),
        generation: 1,
        delay: GRACE,
    };
    assert_eq!(fixture.core.fire_auto_follow_at(timer, Instant::now() + GRACE), None);
    assert_eq!(fixture.selected(), Some(idle_session));
}

// MARK: - revealing the pane that set the status

fn install(fixture: &mut Fixture, id: Uuid, pane: StatusPane, token: &str) {
    let session = fixture.store_mut().unwrap().session_mut(id).unwrap();
    session.pane_slot_mut(pane).replace(Box::new(FakeSurface::new(token)));
}

/// A session with a split (terminals in both panes, primary focused) and a shown scratch terminal.
fn layered() -> (Fixture, Uuid) {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    fixture.session(
        SessionCmd::Split,
        Some(&wire(id)),
        args(|args| args.mode = Some("on".into())),
    );
    install(&mut fixture, id, StatusPane::Left, "L");
    install(&mut fixture, id, StatusPane::Right, "R");
    let store = fixture.store_mut().unwrap();
    store.set_split_focus(id, false);
    store.toggle_scratch(id, true);
    (fixture, id)
}

fn reveal(fixture: &mut Fixture, id: Uuid, status: AgentStatus, pane: Option<StatusPane>) -> (bool, bool) {
    let indicator = AgentIndicator {
        status_pane: pane,
        ..indicator(status)
    };
    fixture.core.reveal_blocked_pane(id, Some(&indicator));
    assert!(fixture.core.pane_focus_requested(id));
    let session = session(fixture, id);
    (session.scratch_active, session.split.focused)
}

#[test]
fn reveal_focuses_the_split_pane_that_blocked() {
    let (mut fixture, id) = layered();
    assert_eq!(
        reveal(&mut fixture, id, AgentStatus::Blocked, Some(StatusPane::Right)),
        (false, true)
    );
}

#[test]
fn reveal_shows_the_scratch_that_blocked() {
    let (mut fixture, id) = layered();
    fixture.store_mut().unwrap().toggle_scratch(id, true);
    assert_eq!(
        reveal(&mut fixture, id, AgentStatus::Completed, Some(StatusPane::Scratch)),
        (true, false)
    );
}

#[test]
fn reveal_lands_an_untagged_block_on_the_primary_pane() {
    let (mut fixture, id) = layered();
    fixture.store_mut().unwrap().set_split_focus(id, true);
    assert_eq!(reveal(&mut fixture, id, AgentStatus::Blocked, None), (false, false));
}

#[test]
fn reveal_leaves_an_active_status_alone() {
    let (mut fixture, id) = layered();
    assert_eq!(
        reveal(&mut fixture, id, AgentStatus::Active, Some(StatusPane::Right)),
        (true, false)
    );
}

#[test]
fn an_open_dashboard_suppresses_the_fire_until_it_closes() {
    let mut fixture = Fixture::new(0);
    let idle_session = fixture.sessions[0];
    let blocked = add_blocked(&mut fixture, 100);
    select(&mut fixture, idle_session);
    let targets = Some(vec![wire(idle_session)]);
    let open = fixture.send(Command::Dashboard, None, args(|a| a.targets = targets));
    assert!(open.ok, "{open:?}");
    assert_eq!(fire(&mut fixture), None);
    assert_eq!(fixture.selected(), Some(idle_session));
    let close = fixture.send(Command::Dashboard, None, args(|a| a.close = Some(true)));
    assert!(close.ok, "{close:?}");
    assert!(fire(&mut fixture).is_some());
    assert_eq!(fixture.selected(), Some(blocked));
}
