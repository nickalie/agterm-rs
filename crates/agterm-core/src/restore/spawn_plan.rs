//! What a session pane spawns with on a relaunch and in which order the launch spawns them (upstream's
//! `Ghostty/LaunchSeed.swift` and `LaunchSpawnPlan.swift`).
//!
//! A pane's seed is decided at SPAWN time, not when its view is built: while a pane waits for its launch spawn permit,
//! the captured argv and the pin stay on the session, so `restore.clear` still disarms them and a clean quit still
//! captures across that window. The seed takes the pane's live role, since `session.swap` moves a terminal and its
//! pending payloads to the other slot together.

use std::collections::{BTreeSet, HashSet};

use uuid::Uuid;

use crate::command_restore::{RestoreInputs, restore_plan, shell_quoted_line, should_restore};
use crate::live::support::{Configuration, attach_command, settled};
use crate::model::{Session, StatusPane, WindowLibrary};

/// What a pane spawns with: the exec `command`, the `initial_input` typed into a login shell, and whether a command's
/// exit holds on the "press any key" prompt (upstream's `LaunchSeed`). `command` and `initial_input` exclude each
/// other.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchSeed {
    pub command: Option<String>,
    pub initial_input: Option<String>,
    pub wait_after_command: bool,
    /// Variables the terminal runs with on top of the pane's own environment: a wrapped pane's zmx configuration.
    pub env: Vec<(String, String)>,
    /// The pane attaches to its live-session daemon (upstream's `backedByZmx`, `backedByLive` on the wire here), fixed
    /// at spawn as its disposition is.
    pub live: bool,
}

/// The launch-wide inputs a seed is decided against (upstream's `LaunchSeedPolicy`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchSeedPolicy {
    /// This launch replays captured commands and pins (`rerun` is active).
    pub restore_enabled: bool,
    /// `live` was requested and fell back: neither pending slot is read, so both stay armed for the next launch.
    pub live_fell_back: bool,
    /// The program basenames never re-run (`restore-denylist.conf`).
    pub denylist: HashSet<String>,
    /// The daemon names the launch reap saw alive, a scheduling hint only: `None` (the listing failed) paces every
    /// replaying live pane, and a name that died between the list and the attach merely spawns unpaced.
    pub running_names: Option<BTreeSet<String>>,
    /// Daemons whose reset could not be confirmed at this launch: their panes attach with neither the captured replay
    /// nor the durable command, so a process that may still run is not started twice.
    pub suppressed_daemons: BTreeSet<String>,
}

impl LaunchSeedPolicy {
    /// The `initial_input` for a captured argv: its shell-quoted line and a newline, or `None` outside rerun mode or
    /// when the argv may not be replayed (denylisted, a control character, an undecodable byte).
    fn restore_initial_input(&self, argv: Option<&[String]>) -> Option<String> {
        let argv = argv.filter(|_| self.restore_enabled)?;
        let denylist: Vec<&str> = self.denylist.iter().map(String::as_str).collect();
        should_restore(argv, &denylist).then(|| format!("{}\n", shell_quoted_line(argv)))
    }
}

/// Pane `pane`'s seed, consuming the slots the launch armed for it (upstream's `LaunchSeedProvider.seed`). The scratch
/// has none.
pub fn resolve_seed(session: &mut Session, pane: StatusPane, policy: &LaunchSeedPolicy) -> LaunchSeed {
    let Some(launch) = session.pane_launch(pane).cloned() else {
        return LaunchSeed::default();
    };
    let inputs = if policy.live_fell_back {
        RestoreInputs {
            was_restored: session.was_restored,
            restore_enabled: false,
            had_foreground: false,
            foreground_input: None,
            initial_command: launch.command,
            restore_override: None,
            requested_wait: launch.wait,
        }
    } else {
        let capture = session.take_pending_foreground(pane);
        RestoreInputs {
            was_restored: session.was_restored,
            restore_enabled: policy.restore_enabled,
            had_foreground: capture.is_some(),
            foreground_input: policy.restore_initial_input(capture.as_deref()),
            initial_command: launch.command,
            restore_override: session.take_pending_restore_override(pane),
            requested_wait: launch.wait,
        }
    };
    let plan = restore_plan(&inputs);
    LaunchSeed {
        command: plan.command,
        initial_input: plan.initial_input,
        wait_after_command: plan.wait_after_command,
        ..LaunchSeed::default()
    }
}

/// Pane `pane`'s seed under the wrapped live disposition `configuration` (upstream's `ZmxLaunch.surfaceSeed`): the
/// attach, carrying the pane's captured argv as the payload a new daemon runs, else its durable creation command. The
/// capture is consumed either way; a suppressed daemon attaches with neither. The pending `session.restore` pin is
/// never read here: a daemon that survived runs its own program. The attach's exit always closes the pane.
pub fn resolve_live_seed(
    session: &mut Session,
    pane: StatusPane,
    policy: &LaunchSeedPolicy,
    configuration: &Configuration,
) -> LaunchSeed {
    let suppressed = policy.suppressed_daemons.contains(&configuration.daemon_name);
    let captured = session.take_pending_foreground(pane);
    let replay = captured.filter(|_| !suppressed);
    let creation = if replay.is_none() && !suppressed {
        session.pane_launch(pane).and_then(|launch| launch.command.clone())
    } else {
        None
    };
    let denylist: Vec<&str> = policy.denylist.iter().map(String::as_str).collect();
    LaunchSeed {
        command: Some(settled(&attach_command(
            configuration,
            replay.as_deref(),
            creation.as_deref(),
            &denylist,
        ))),
        initial_input: None,
        wait_after_command: false,
        env: configuration.environment.clone().into_iter().collect(),
        live: true,
    }
}

/// [`should_pace`] under the wrapped live disposition `configuration`: a restored pane whose daemon the reap saw alive
/// is attached to, which runs no program, and a suppressed one runs none either; otherwise it paces when its captured
/// argv may replay, or else when it has a durable command.
#[must_use]
pub fn should_pace_live(
    session: &Session,
    pane: StatusPane,
    policy: &LaunchSeedPolicy,
    configuration: &Configuration,
) -> bool {
    if !session.was_restored {
        return false;
    }
    let name = &configuration.daemon_name;
    if policy.running_names.as_ref().is_some_and(|names| names.contains(name))
        || policy.suppressed_daemons.contains(name)
    {
        return false;
    }
    let denylist: Vec<&str> = policy.denylist.iter().map(String::as_str).collect();
    match session
        .pane_restore(pane)
        .and_then(|restore| restore.pending_foreground.as_deref())
    {
        Some(capture) => should_restore(capture, &denylist),
        None => session.pane_launch(pane).is_some_and(|launch| launch.command.is_some()),
    }
}

/// Whether pane `pane`'s seed would start a program, read without consuming anything (upstream's
/// `LaunchSeedProvider.shouldPace`). Only a restored pane is paced: a fresh or runtime pane never waits for a permit.
#[must_use]
pub fn should_pace(session: &Session, pane: StatusPane, policy: &LaunchSeedPolicy) -> bool {
    if !session.was_restored || policy.live_fell_back {
        return false;
    }
    let (Some(launch), Some(restore)) = (session.pane_launch(pane), session.pane_restore(pane)) else {
        return false;
    };
    let capture = restore.pending_foreground.as_deref();
    let plan = restore_plan(&RestoreInputs {
        was_restored: true,
        restore_enabled: policy.restore_enabled,
        had_foreground: capture.is_some(),
        foreground_input: policy.restore_initial_input(capture),
        initial_command: launch.command.clone(),
        restore_override: restore.pending_command.clone(),
        requested_wait: false,
    });
    plan.command.is_some() || plan.initial_input.is_some()
}

/// The expected spawn order of a launch restore and the keys exempt from pacing (upstream's `LaunchSpawnPlan`): each
/// open window's selected session's on-screen panes come first and form the burst; the rest follow window by window,
/// session by session, primary before shown split. A hidden split and a session that was not restored are never
/// expected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchSpawnPlan {
    pub order: Vec<Uuid>,
    pub burst: HashSet<Uuid>,
}

impl LaunchSpawnPlan {
    /// The plan over every open window of `library`, in window order.
    #[must_use]
    pub fn of(library: &WindowLibrary) -> Self {
        let mut burst = Vec::new();
        let mut rest = Vec::new();
        for store in library.open_ids().into_iter().filter_map(|id| library.store(id)) {
            let selected = store.selected_session_id();
            for session in store.sessions().filter(|session| session.was_restored) {
                let keys = Self::panes(session);
                if Some(session.id()) == selected {
                    burst.extend(keys);
                } else {
                    rest.extend(keys);
                }
            }
        }
        let burst_set = burst.iter().copied().collect();
        burst.extend(rest);
        Self {
            order: burst,
            burst: burst_set,
        }
    }

    /// A session's expected keys: the shown split's too, never a hidden one's, which has no pane to request.
    fn panes(session: &Session) -> Vec<Uuid> {
        match session.split_pane_identity.filter(|_| session.split.shown) {
            Some(split) => vec![session.pane_identity, split],
            None => vec![session.pane_identity],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewSession, SessionSeed};

    fn policy(restore_enabled: bool, denylist: &[&str]) -> LaunchSeedPolicy {
        LaunchSeedPolicy {
            restore_enabled,
            live_fell_back: false,
            denylist: denylist.iter().map(|name| (*name).to_owned()).collect(),
            ..LaunchSeedPolicy::default()
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn session(command: Option<&str>, wait: bool, restored: bool) -> Session {
        let mut session = Session::new(SessionSeed {
            initial_cwd: "/tmp".to_owned(),
            ..SessionSeed::default()
        });
        session.launch.command = command.map(str::to_owned);
        session.launch.wait = wait;
        session.was_restored = restored;
        session
    }

    #[test]
    fn a_fresh_command_session_always_runs_its_command_and_holds_when_asked() {
        let mut fresh = session(Some("cat"), true, false);
        let seed = resolve_seed(&mut fresh, StatusPane::Left, &policy(false, &[]));
        assert_eq!(
            seed,
            LaunchSeed {
                command: Some("cat".to_owned()),
                initial_input: None,
                wait_after_command: true,
                ..LaunchSeed::default()
            }
        );
        assert!(!should_pace(&fresh, StatusPane::Left, &policy(true, &[])));
    }

    #[test]
    fn a_restored_command_session_reruns_only_in_rerun_mode() {
        let mut restored = session(Some("cat"), true, true);
        assert_eq!(
            resolve_seed(&mut restored, StatusPane::Left, &policy(false, &[])),
            LaunchSeed::default()
        );
        assert!(should_pace(&restored, StatusPane::Left, &policy(true, &[])));
        assert!(!should_pace(&restored, StatusPane::Left, &policy(false, &[])));
        let seed = resolve_seed(&mut restored, StatusPane::Left, &policy(true, &[]));
        assert_eq!(seed.command.as_deref(), Some("cat"));
        assert!(seed.wait_after_command);
    }

    #[test]
    fn a_captured_job_is_typed_once_and_preempts_the_creation_command() {
        let mut restored = session(Some("/bin/sh"), false, true);
        restored.restore.pending_foreground = Some(argv(&["sleep", "600"]));
        let rerun = policy(true, &[]);
        assert!(should_pace(&restored, StatusPane::Left, &rerun));
        let seed = resolve_seed(&mut restored, StatusPane::Left, &rerun);
        assert_eq!(seed.command, None);
        assert_eq!(seed.initial_input.as_deref(), Some("'sleep' '600'\n"));
        assert_eq!(restored.restore.pending_foreground, None, "consumed once");
        assert_eq!(
            resolve_seed(&mut restored, StatusPane::Left, &rerun).command.as_deref(),
            Some("/bin/sh")
        );
    }

    #[test]
    fn a_denylisted_capture_still_preempts_and_leaves_a_plain_shell() {
        let mut restored = session(Some("tmux"), false, true);
        restored.restore.pending_foreground = Some(argv(&["tmux", "attach"]));
        let rerun = policy(true, &["tmux"]);
        assert!(!should_pace(&restored, StatusPane::Left, &rerun));
        assert_eq!(
            resolve_seed(&mut restored, StatusPane::Left, &rerun),
            LaunchSeed::default()
        );
    }

    #[test]
    fn a_pin_is_typed_verbatim_in_rerun_and_ignored_in_fresh_shells() {
        let mut restored = session(None, false, true);
        restored.restore.pending_command = Some("cd x && claude --resume y".to_owned());
        assert!(!should_pace(&restored, StatusPane::Left, &policy(false, &[])));
        let mut fresh = session(None, false, true);
        fresh.restore.pending_command = Some("echo pinned".to_owned());
        assert_eq!(
            resolve_seed(&mut fresh, StatusPane::Left, &policy(false, &[])),
            LaunchSeed::default()
        );
        let seed = resolve_seed(&mut restored, StatusPane::Left, &policy(true, &[]));
        assert_eq!(seed.initial_input.as_deref(), Some("cd x && claude --resume y\n"));
        assert_eq!(seed.command, None);
    }

    #[test]
    fn a_live_fallback_reads_no_slot_and_holds_the_creation_command() {
        let mut restored = session(Some("cat"), false, true);
        restored.restore.pending_foreground = Some(argv(&["vim"]));
        restored.restore.pending_command = Some("echo pinned".to_owned());
        let fallback = LaunchSeedPolicy {
            restore_enabled: false,
            live_fell_back: true,
            ..LaunchSeedPolicy::default()
        };
        assert_eq!(
            resolve_seed(&mut restored, StatusPane::Left, &fallback),
            LaunchSeed::default()
        );
        assert!(
            restored.restore.pending_foreground.is_some(),
            "kept for the next launch"
        );
        assert!(restored.restore.pending_command.is_some());
    }

    #[test]
    fn the_plan_bursts_the_selected_sessions_shown_panes_first() {
        let mut library = WindowLibrary::new("/tmp");
        let window = library.new_window(None);
        let store = library.store_mut(window).expect("the store");
        let workspace = store.add_workspace("work");
        let first = store
            .add_session(workspace, NewSession::at_cwd("/tmp"))
            .expect("a session");
        let second = store
            .add_session(workspace, NewSession::at_cwd("/tmp"))
            .expect("a session");
        let fresh = store
            .add_session(workspace, NewSession::at_cwd("/tmp"))
            .expect("a session");
        let split = Uuid::new_v4();
        for id in [first, second] {
            store.session_mut(id).expect("restored").was_restored = true;
        }
        let selected = store.session_mut(second).expect("selected");
        selected.split_pane_identity = Some(split);
        selected.split.shown = true;
        store.select_session(Some(second));
        let hidden = store.session_mut(first).expect("hidden split");
        hidden.split_pane_identity = Some(Uuid::new_v4());
        let store = library.store(window).expect("the store");
        let key = |id: Uuid| store.session(id).expect("a session").pane_identity;
        let plan = LaunchSpawnPlan::of(&library);
        assert_eq!(plan.order, vec![key(second), split, key(first)]);
        assert_eq!(plan.burst, HashSet::from([key(second), split]));
        assert!(!plan.order.contains(&key(fresh)));
    }
}
