//! The live half of `session.restart` (upstream's `restartSessionPane` in `ControlServer+Restart.swift`): ends a live
//! pane's daemon and builds the pane a new terminal whose daemon runs the caller's line, or the pane's foreground
//! program again, under the same pane identity, so the daemon keeps its name.
//!
//! The steps wait on the daemons, each probe a main-loop hop apart ([`Dispatched::Pending`]): the old shell is listed
//! first (a pane just created reads as backed before its daemon exists), every replay refusal is decided before
//! anything is ended, the kill must be confirmed, the old program gets a second to act on the hangup and is then
//! killed, and only once it is gone does the new terminal start, so it cannot meet a held port or lock. The pane's exit
//! is claimed from the kill on, so the dying client cannot close the pane the restart rebuilds.

use std::path::Path;
use std::time::{Duration, Instant};

use agterm_host::{KillOutcome, ProcessRecord, Surface};
use agterm_proto::{ControlRestartReceipt, ControlResult, Response};
use uuid::Uuid;

use super::zmx::launch::live_foreground_group;
use crate::Core;
use crate::command_restore::shell_quoted_line;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::model::StatusPane;
use crate::pane_env::uuid_string;
use crate::restart_replay::{self, Launch, Observation};
use crate::restore::capture::ForegroundReader;
use crate::zmx::lead::LeadAttachment;
use crate::zmx::support::attach_command;

/// How long the old shell may take to show up in the listing.
const SHELL_WAIT: Duration = Duration::from_secs(5);
/// How long the new shell may take to show up.
const RESTART_WAIT: Duration = Duration::from_secs(10);
/// How long the old program has to act on the hangup before it is killed, and then to die of that.
const HANGUP_GRACE: Duration = Duration::from_secs(1);
const KILL_GRACE: Duration = Duration::from_millis(500);
/// The pause between probes.
const POLL: Duration = Duration::from_millis(100);

/// One restart in flight.
pub(crate) struct Job {
    pub(crate) session: Uuid,
    pub(crate) pane: StatusPane,
    /// The pane's token, its identity spelled as `AGTERM_PANE_ID`.
    pub(crate) token: String,
    pub(crate) identity: Uuid,
    pub(crate) daemon: String,
    /// The caller's line; `None` replays the foreground program.
    pub(crate) command: Option<String>,
}

/// Starts the live restart of `job`'s pane.
pub(crate) fn begin(core: &mut Core, job: Job) -> Dispatched {
    if core.wrap_configuration(job.identity, None).is_none() {
        return failure("live sessions are unavailable for this pane");
    }
    let deadline = Instant::now() + SHELL_WAIT;
    find_old_shell(core, job, deadline)
}

fn failure(message: impl Into<String>) -> Dispatched {
    Dispatched::Done(Response::from(ControlError::rejected(message)))
}

fn later(step: impl FnOnce(&mut Core) -> Dispatched + Send + 'static) -> Dispatched {
    Dispatched::Pending {
        after: POLL,
        resume: Box::new(step),
    }
}

/// The leader pid the listing reports for `daemon` once it differs from `other_than`.
fn listed_shell(core: &Core, daemon: &str, other_than: Option<i32>) -> Option<i32> {
    let records = core.zmx.host()?.list().ok()?;
    records
        .into_iter()
        .find(|record| record.name == daemon)?
        .leader_pid
        .filter(|pid| Some(*pid) != other_than)
}

fn find_old_shell(core: &mut Core, job: Job, deadline: Instant) -> Dispatched {
    match listed_shell(core, &job.daemon, None) {
        Some(old) => end_old_shell(core, job, old),
        None if Instant::now() < deadline => later(move |core| find_old_shell(core, job, deadline)),
        None => failure("the pane's shell is not running"),
    }
}

/// Whether the pane still holds its terminal in an open window's tree: a session waiting out its undo window is in
/// no open tree, and killing its shell would leave undo a dead pane to restore.
fn pane_unchanged(core: &Core, job: &Job) -> bool {
    core.library()
        .store_for_session(job.session)
        .and_then(|store| store.session(job.session))
        .and_then(|session| session.pane_role_for_token(&job.token))
        == Some(job.pane)
}

fn end_old_shell(core: &mut Core, job: Job, old: i32) -> Dispatched {
    if !pane_unchanged(core, &job) {
        return failure("the pane changed before the restart; nothing was started");
    }
    let replay = match &job.command {
        Some(_) => None,
        None => match replay_launch(core, &job, old) {
            Ok(launch) => Some(launch),
            Err(message) => return failure(message),
        },
    };
    let Some(host) = core.zmx.host().cloned() else {
        return failure("live sessions are unavailable for this pane");
    };
    // read before the kill: without it the restart could not tell when the old program is gone
    let Some(program) = host.foreground_job(old) else {
        return failure(
            "the process table cannot be read, so the old program could not be tracked; nothing was changed",
        );
    };
    match host.kill(&job.daemon, true) {
        KillOutcome::Killed => {}
        KillOutcome::StaleSocket => {
            return failure(format!("{} did not confirm the kill; nothing was started", job.daemon));
        }
        KillOutcome::Failed(reason) => {
            return failure(format!("could not end the pane's shell: {reason}; nothing was started"));
        }
    }
    // the dying client's exit would close the pane, and its `unowned` report would reattach to a daemon that is gone
    core.zmx.claimed_exits.insert(job.token.clone());
    core.zmx.leads.forget(job.identity);
    core.zmx.leaders.borrow_mut().note_lifecycle_change();
    let ending = Ending {
        old,
        program,
        replay,
        deadline: Instant::now() + HANGUP_GRACE,
        killed: false,
    };
    await_program_end(core, job, ending)
}

/// What the restart waits on between the kill and the new terminal.
struct Ending {
    old: i32,
    program: Vec<ProcessRecord>,
    replay: Option<Launch>,
    deadline: Instant,
    /// The old program outlived the hangup grace and was killed.
    killed: bool,
}

fn await_program_end(core: &mut Core, job: Job, mut ending: Ending) -> Dispatched {
    let running = core.zmx.host().is_some_and(|host| host.job_running(&ending.program));
    if !running {
        return rebuild(core, job, &ending);
    }
    if Instant::now() < ending.deadline {
        return later(move |core| await_program_end(core, job, ending));
    }
    if !ending.killed {
        if let Some(host) = core.zmx.host() {
            host.end_job(&ending.program);
        }
        ending.killed = true;
        ending.deadline = Instant::now() + KILL_GRACE;
        return later(move |core| await_program_end(core, job, ending));
    }
    close_ended_pane(core, &job);
    failure(format!(
        "the old shell ended (pid {}) but its program is still running; nothing was started and the pane was closed",
        ending.old
    ))
}

fn rebuild(core: &mut Core, job: Job, ending: &Ending) -> Dispatched {
    let old = ending.old;
    if !pane_unchanged(core, &job) {
        close_ended_pane(core, &job);
        return failure(format!(
            "the old shell ended (pid {old}) and the pane changed during the restart; nothing was started"
        ));
    }
    let attachment = LeadAttachment::new(false);
    let Some(configuration) = core.wrap_configuration(job.identity, Some(attachment.clone())) else {
        close_ended_pane(core, &job);
        return failure(format!(
            "the old shell ended (pid {old}) and the pane could not be rebuilt; it was closed"
        ));
    };
    let Some(store) = core.library_mut().store_for_session_mut(job.session) else {
        close_ended_pane(core, &job);
        return failure(format!(
            "the old shell ended (pid {old}) and the pane changed during the restart; nothing was started"
        ));
    };
    store.clear_pane_owned_state(job.session, job.pane);
    let cwd = store
        .session(job.session)
        .map(|session| session.cwd_for(job.pane).to_owned())
        .filter(|cwd| Path::new(cwd).is_dir());
    // the denylist was applied before the kill: a rejection here would start a plain shell instead
    let none: [&str; 0] = [];
    let command = attach_command(
        &configuration,
        ending.replay.as_ref().map(|launch| launch.argv.as_slice()),
        job.command.as_deref(),
        &none,
    );
    core.begin_replacement_lead(&job.token, job.identity, attachment, false);
    core.request_pane_replacement(super::zmx::PaneReplacement {
        session: job.session,
        token: job.token.clone(),
        command: crate::zmx::support::settled(&command),
        env: configuration.environment.into_iter().collect(),
        cwd: ending
            .replay
            .as_ref()
            .map(|launch| launch.working_directory.clone())
            .or(cwd),
    });
    let replayed = ending.replay.as_ref().map(|launch| launch.argv.clone());
    let deadline = Instant::now() + RESTART_WAIT;
    later(move |core| await_new_shell(core, job, old, replayed, deadline))
}

fn await_new_shell(
    core: &mut Core,
    job: Job,
    old: i32,
    replayed: Option<Vec<String>>,
    deadline: Instant,
) -> Dispatched {
    // the claim covered the old terminal from the kill until the frontend installed the replacement, which it did in
    // the settle right after the rebuild, claiming the old terminal's exit itself; the new terminal's own exit must
    // close the pane (upstream claims per surface), so the claim ends at the first probe after the rebuild
    core.zmx.claimed_exits.remove(&job.token);
    let Some(new) = listed_shell(core, &job.daemon, Some(old)) else {
        // a replacement whose attach already exited closed the pane: no new shell will show up
        if Instant::now() < deadline && pane_unchanged(core, &job) {
            return later(move |core| await_new_shell(core, job, old, replayed, deadline));
        }
        return failure(format!("the old shell ended (pid {old}) and no new one was observed"));
    };
    core.zmx.leaders.borrow_mut().note_lifecycle_change();
    let pane = job.pane.as_str();
    let note = replayed
        .as_ref()
        .map(|argv| format!("; replay requested: {}", shell_quoted_line(argv)))
        .unwrap_or_default();
    Dispatched::done(Ok(ControlResult {
        id: Some(uuid_string(job.session)),
        text: Some(format!("restarted {pane} pane: shell {old} -> {new}{note}")),
        pane: Some(pane.to_owned()),
        restart: Some(ControlRestartReceipt {
            pane_id: job.token,
            old_pid: old,
            new_pid: new,
            replayed_argv: replayed,
        }),
        ..ControlResult::default()
    }))
}

/// The foreground program to run again, read through the daemon's shell (upstream's `replayLaunch`).
fn replay_launch(core: &Core, job: &Job, old: i32) -> Result<Launch, String> {
    let unreadable = || restart_replay::Refusal::Unreadable.message();
    let host = core.zmx.host().ok_or_else(unreadable)?;
    // a failed listing drops the cached leaders, so a daemon that is gone cannot answer for the pane
    core.zmx
        .leaders
        .borrow_mut()
        .accept(host.list().ok().map(|records| crate::zmx::lifecycle::leaders(&records)));
    let table = core.restore.table().ok_or_else(unreadable)?;
    let live = |token: &str| live_foreground_group(&core.zmx, table, token);
    let reader = ForegroundReader::new(table, core.restore.shell()).with_live(&live);
    let observed = core
        .library()
        .store_for_session(job.session)
        .and_then(|store| store.session(job.session))
        .and_then(|session| session.pane_slot(job.pane).get())
        .and_then(|surface: &dyn Surface| reader.group_of(surface))
        .and_then(|pgid| reader.observed(pgid));
    let working_directory = observed
        .as_ref()
        .and_then(|(pid, _)| table.working_directory(*pid))
        .filter(|directory| Path::new(directory).is_dir());
    let observation = Observation {
        is_daemon_leader: observed.as_ref().is_some_and(|(pid, _)| *pid == old),
        foreground: observed.map(|(_, foreground)| foreground),
        working_directory,
    };
    let denylist: Vec<&str> = core.restore.denylist().iter().map(String::as_str).collect();
    restart_replay::resolve(&observation, core.restore.shell(), &denylist).map_err(|refusal| refusal.message())
}

/// Runs the exit transition the restart claimed, for one that stops after its kill: the pane has no shell, and with
/// its exit claimed nothing else would close it. A session soft-closed meanwhile is made final instead, since undo
/// would bring back that dead pane.
fn close_ended_pane(core: &mut Core, job: &Job) {
    core.zmx.claimed_exits.remove(&job.token);
    core.note_already_finalized(job.identity);
    if let Some(store) = core.library_mut().store_for_session_mut(job.session) {
        match store
            .session(job.session)
            .and_then(|session| session.pane_role_for_token(&job.token))
        {
            Some(StatusPane::Left) => store.close_primary_pane(job.session),
            Some(StatusPane::Right) => store.close_split_pane(job.session),
            _ => {}
        }
        return;
    }
    let library = core.library_mut();
    if let Some(window) = library.window_holding_session(job.session)
        && let Some(store) = library.store_mut(window)
    {
        store.finalize_pending_close_of_session(job.session);
    }
}
