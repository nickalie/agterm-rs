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

impl Core {
    /// The search bar's field changed to `needle` in window `window` (upstream's `updateSearchNeedle`): the active
    /// session's needle mirrors it and goes to the pane the open search is pinned to; an empty needle blanks the
    /// counter at once rather than waiting out libghostty's teardown. Commit afterwards.
    pub fn search_bar_needle(&mut self, window: Uuid, needle: &str) {
        let Some(session) = active_search(self, window) else {
            return;
        };
        needle.clone_into(&mut session.search.needle);
        if needle.is_empty() {
            session.search.total = None;
            session.search.selected = None;
        }
        if let Some(owner) = session.search.owner {
            run_binding(session, owner, &format!("search:{needle}"));
        }
    }

    /// The search bar stepped to the next match (its down button, Return) or the previous one (its up button,
    /// Shift+Return) in window `window` (upstream's `navigateSearch`). Commit afterwards.
    pub fn search_bar_step(&mut self, window: Uuid, next: bool) {
        let Some(session) = active_search(self, window) else {
            return;
        };
        let step = if next { Step::Next } else { Step::Prev };
        if let Some(owner) = session.search.owner {
            run_binding(session, owner, step.action());
        }
    }

    /// The search bar closed in window `window` (its close button, Escape): search really ends on its pane, whose end
    /// report clears the state and gives the terminal the keyboard back (upstream's `endSearch`). Commit afterwards.
    pub fn search_bar_close(&mut self, window: Uuid) {
        let Some(session) = active_search(self, window) else {
            return;
        };
        if let Some(owner) = session.search.owner {
            run_binding(session, owner, "end_search");
        }
    }

    /// A zoom began in window `window` on a terminal of session `zoomed`: an open search ends on the active session
    /// and on the zoomed one, or libghostty keeps highlighting matches with no bar to show them (upstream's
    /// `handleZoomTargetChange`).
    pub(crate) fn end_search_for_zoom(&mut self, window: Uuid, zoomed: Uuid) {
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        let active = store.selected_session_id();
        for id in active.into_iter().chain((active != Some(zoomed)).then_some(zoomed)) {
            let Some(session) = store.session_mut(id).filter(|session| session.search.active) else {
                continue;
            };
            if let Some(owner) = session.search.owner {
                run_binding(session, owner, "end_search");
            }
        }
    }
}

/// Window `window`'s active session while its search is open.
fn active_search(core: &mut Core, window: Uuid) -> Option<&mut Session> {
    let store = core.library_mut().store_mut(window)?;
    let id = store.selected_session_id()?;
    store.session_mut(id).filter(|session| session.search.active)
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
    if let Some(error) = core.modals.pending_modal_error(resolved.window) {
        return Err(ControlError::rejected(error));
    }
    if core.zoom_target(resolved.window).is_some() {
        return Err(ControlError::rejected("terminal zoom active"));
    }
    // the pinned owner when a search is open, else the pane an open would land on (`--to close` stays available)
    refuse_covered(core, resolved.session, None)?;
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

/// The refusal of a search on a lead-covered pane (upstream's `coveredRefusal`): its surface holds output laid out for
/// another client's grid, so a count from it would be of the wrong text. The pane is the pinned owner when a search
/// is open, else `pane`, else the on-screen one; the role can change across every wait, so it is checked before each
/// mutation and before the answer.
fn refuse_covered(core: &Core, id: Uuid, pane: Option<StatusPane>) -> Result<(), ControlError> {
    let Some(session) = core.library().store_for_session(id).and_then(|store| store.session(id)) else {
        return Ok(());
    };
    let pane = session
        .search
        .owner
        .or(pane)
        .unwrap_or_else(|| session.on_screen_pane());
    match session
        .pane_slot(pane)
        .get()
        .and_then(|surface| core.covered_refusal(surface))
    {
        Some(refusal) => Err(ControlError::rejected(refusal)),
        None => Ok(()),
    }
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
    if let Err(refusal) = refuse_covered(core, job.run.session, Some(pane)) {
        return Dispatched::done(Err(refusal));
    }
    let Ok(session) = session_mut(core, job.run.session) else {
        return Dispatched::done(Err(not_realized()));
    };
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
    if let Err(refusal) = refuse_covered(core, run.session, Some(owner)) {
        return Dispatched::done(Err(refusal));
    }
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
    if let Err(refusal) = refuse_covered(core, run.session, Some(owner)) {
        return Dispatched::done(Err(refusal));
    }
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
    if let Err(refusal) = refuse_covered(core, id, None) {
        return Dispatched::done(Err(refusal));
    }
    let Ok(session) = session_mut(core, id) else {
        return Dispatched::done(Err(not_realized()));
    };
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
