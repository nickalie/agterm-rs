//! `session.search`: in-terminal search driven over the control channel, mirroring the search bar (upstream's
//! `searchSession` in `agterm/Control/ControlServer+SurfaceIO.swift`).
//!
//! `--to close` ends the open search without selecting anything. Otherwise the session is selected, its on-screen
//! pane waited for, search opened on it unless open already, the needle sent and the selection stepped, all on the
//! pane the open search is pinned to. libghostty reports the match count asynchronously, so the answer waits for it
//! (at most [`COUNT_ATTEMPTS`] probes [`POLL_INTERVAL`] apart) and carries the count and the counter text.

use agterm_proto::{ControlResult, Request, Response};
use uuid::Uuid;

use super::surface::{POLL_INTERVAL, not_realized, pane_realized, run_binding};
use super::typing::ATTEMPTS as REALIZE_ATTEMPTS;
use super::{args_of, with_id};
use crate::Core;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::model::{Session, StatusPane};
use crate::resolve::Resolver;

/// How many times the count is probed (upstream's 16 × 0.03 s).
const COUNT_ATTEMPTS: u32 = 16;

/// Where `--to` steps the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Next,
    Prev,
}

impl Step {
    /// libghostty's `navigate_search` walks the opposite way of agterm's convention (`next` = visually down, toward
    /// newer output), so the directions swap (upstream's `SearchDirection.ghosttyAction`).
    fn action(self) -> &'static str {
        match self {
            Self::Next => "navigate_search:previous",
            Self::Prev => "navigate_search:next",
        }
    }
}

/// A search waiting on its pane or on libghostty's count.
struct SearchJob {
    run: SearchRun,
    text: Option<String>,
}

/// What every step of a search needs after the needle went out.
#[derive(Debug, Clone, Copy)]
struct SearchRun {
    session: Uuid,
    step: Option<Step>,
}

/// Starts `session.search`.
pub(crate) fn begin(core: &mut Core, request: &Request) -> Dispatched {
    start(core, request).unwrap_or_else(|error| Dispatched::Done(Response::from(error)))
}

fn start(core: &mut Core, request: &Request) -> Result<Dispatched, ControlError> {
    let args = args_of(request);
    // the target is resolved before `--to` is checked: an unknown session wins
    let resolved = Resolver::new(core.library()).session(request.target.as_deref(), args.window.as_deref())?;
    let step = match args.to.as_deref() {
        None => None,
        Some("next") => Some(Step::Next),
        Some("prev") => Some(Step::Prev),
        Some("close") => return Ok(Dispatched::done(close(core, resolved.session))),
        Some(_) => return Err(ControlError::rejected("session.search --to must be next|prev|close")),
    };
    if core.zoom_target(resolved.window).is_some() {
        return Err(ControlError::rejected("terminal zoom active"));
    }
    if let Some(store) = core.library_mut().store_mut(resolved.window) {
        store.select_session(Some(resolved.session));
    }
    let job = SearchJob {
        run: SearchRun {
            session: resolved.session,
            step,
        },
        text: args.text.clone(),
    };
    Ok(open(core, job, 0))
}

/// `--to close`: ends the search on its pinned pane; with none open there is nothing to end.
fn close(core: &mut Core, id: Uuid) -> Result<ControlResult, ControlError> {
    let session = session_mut(core, id)?;
    if let Some(owner) = session.search.owner {
        run_binding(session, owner, "end_search");
    }
    Ok(with_id(id))
}

fn session_mut(core: &mut Core, id: Uuid) -> Result<&mut Session, ControlError> {
    core.library_mut()
        .store_for_session_mut(id)
        .and_then(|store| store.session_mut(id))
        .ok_or_else(|| ControlError::rejected("no such session"))
}

fn after(step: impl FnOnce(&mut Core) -> Dispatched + Send + 'static) -> Dispatched {
    Dispatched::Pending {
        after: POLL_INTERVAL,
        resume: Box::new(step),
    }
}

/// Opens search on the on-screen pane once its terminal is up (a never-shown session realizes a beat after the
/// select), then sends the needle.
fn open(core: &mut Core, job: SearchJob, attempt: u32) -> Dispatched {
    let session = match session_mut(core, job.run.session) {
        Ok(session) => session,
        Err(error) => return Dispatched::done(Err(error)),
    };
    let pane = session.on_screen_pane();
    if !pane_realized(session, pane) {
        return if attempt < REALIZE_ATTEMPTS {
            after(move |core| open(core, job, attempt + 1))
        } else {
            Dispatched::done(Err(not_realized()))
        };
    }
    if !session.search.active {
        run_binding(session, pane, "start_search");
    }
    let owner = session.search.owner.unwrap_or(pane);
    let run = job.run;
    match job.text {
        // an older query's count may still be on its way: let it land before clearing, so the wait below is for
        // this needle's; an unchanged needle gets no fresh count from libghostty, so nothing is cleared
        Some(text) if text != session.search.needle => after(move |core| query(core, run, owner, &text, true)),
        Some(text) => query(core, run, owner, &text, false),
        None => navigate(core, run, owner),
    }
}

/// Sends the needle; an empty one clears the query and answers at once, as libghostty reports no count for it.
fn query(core: &mut Core, run: SearchRun, owner: StatusPane, text: &str, clear: bool) -> Dispatched {
    let session = match session_mut(core, run.session) {
        Ok(session) => session,
        Err(error) => return Dispatched::done(Err(error)),
    };
    if clear {
        session.search.total = None;
        session.search.selected = None;
    }
    text.clone_into(&mut session.search.needle);
    let owner = session.search.owner.unwrap_or(owner);
    run_binding(session, owner, &format!("search:{text}"));
    if text.is_empty() {
        session.search.total = None;
        session.search.selected = None;
        return Dispatched::done(Ok(with_id(run.session)));
    }
    navigate(core, run, owner)
}

/// Steps the selection, then waits for the count.
fn navigate(core: &mut Core, run: SearchRun, owner: StatusPane) -> Dispatched {
    if let Some(step) = run.step
        && let Ok(session) = session_mut(core, run.session)
    {
        let owner = session.search.owner.unwrap_or(owner);
        run_binding(session, owner, step.action());
    }
    after(move |core| await_count(core, run.session, 0))
}

fn await_count(core: &mut Core, id: Uuid, attempt: u32) -> Dispatched {
    let session = match session_mut(core, id) {
        Ok(session) => session,
        Err(error) => return Dispatched::done(Err(error)),
    };
    if session.search.total.is_none() && attempt + 1 < COUNT_ATTEMPTS {
        return after(move |core| await_count(core, id, attempt + 1));
    }
    // an empty counter (no query ran yet) is left out, so the CLI prints `ok` rather than a blank line
    let display = session.search.display_text();
    Dispatched::done(Ok(ControlResult {
        text: (!display.is_empty()).then_some(display),
        count: session
            .search
            .total
            .map(|total| i64::try_from(total).unwrap_or(i64::MAX)),
        ..ControlResult::default()
    }))
}
