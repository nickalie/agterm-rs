//! `session.type`: text typed into a pane as keystrokes (upstream's `typeSession` and `injectText` in
//! `agterm/Control/ControlServer+SessionActions.swift` and `+SurfaceIO.swift`).
//!
//! `ok` means the keystrokes were queued to the pane's pty, not that its program read them. The main pane of a session
//! created moments ago may not have its terminal yet: the command then optionally selects the session and probes
//! again every [`POLL_INTERVAL`], [`ATTEMPTS`] times, before answering `session not realized`. A `--pane-id` the
//! wait follows into whichever slot holds that terminal at each probe, so a swap cannot hand the keystrokes to
//! another one.

use agterm_proto::{ControlResult, Request, Response};
use uuid::Uuid;

use super::live::PaneSource;
use super::surface::{POLL_INTERVAL, no_scratch, no_split_pane, not_realized};
use super::{Outcome, args_of, parse_surface_pane, session_store, unknown_pane_id, with_id};
use crate::Core;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::keystrokes::{self, KeystrokeSegment};
use crate::model::{PaneAddress, StatusPane, StatusReset};

/// How many times the main pane is probed after the first miss (upstream's 12 × 0.03 s).
pub(crate) const ATTEMPTS: u32 = 12;

/// A `session.type` waiting for its pane's terminal.
struct TypeJob {
    session: Uuid,
    text: String,
    /// The pane id the pane was resolved from, followed at each probe.
    token: Option<String>,
    reset: StatusReset,
}

/// Starts `session.type`: answers at once unless the main pane has no terminal yet.
pub(crate) fn begin(core: &mut Core, request: &Request) -> Dispatched {
    start(core, request).unwrap_or_else(|error| Dispatched::Done(Response::from(error)))
}

fn start(core: &mut Core, request: &Request) -> Result<Dispatched, ControlError> {
    let args = args_of(request);
    let text = args
        .text
        .clone()
        .ok_or_else(|| ControlError::rejected("session.type requires text"))?;
    if text.contains('\0') {
        return Err(ControlError::rejected("text must not contain a NUL byte"));
    }
    let pane = parse_surface_pane(args.pane.as_deref())?;
    let reset = core.settings().effective_status_reset();
    let (store, id) = session_store(core, request.target.as_deref(), args.window.as_deref())?;
    let session = store.session(id).ok_or_else(not_realized)?;
    let pane = match session.pane_address(args.pane_id.as_deref(), pane) {
        PaneAddress::UnknownToken(token) => return Err(unknown_pane_id(&token)),
        PaneAddress::Pane(pane) => pane,
    };
    // only a token that resolved is followed; one that fell back to `--pane` is not
    let token = args
        .pane_id
        .clone()
        .filter(|token| session.pane_role_for_token(token).is_some());
    match pane {
        Some(pane @ (StatusPane::Right | StatusPane::Scratch)) => {
            if !store
                .session(id)
                .is_some_and(|session| session.pane_slot(pane).is_present())
            {
                return Err(if pane == StatusPane::Right {
                    no_split_pane()
                } else {
                    no_scratch()
                });
            }
            // a dormant split spawns now; there being no poll for it, this call still fails fast on it
            if pane == StatusPane::Right {
                core.wake_session(id);
            }
            Ok(Dispatched::done(typed_result(core, id, pane, &text, reset)))
        }
        None | Some(StatusPane::Left) => {
            if type_into(core, id, StatusPane::Left, &text, reset)? {
                return Ok(Dispatched::done(Ok(answer(id, StatusPane::Left))));
            }
            if args.select == Some(true)
                && let Some(store) = core.library_mut().store_for_session_mut(id)
            {
                store.select_session(Some(id));
            }
            let job = TypeJob {
                session: id,
                text,
                token,
                reset,
            };
            // a dormant pane spawns now, so the poll waits only for the terminal
            core.wake_session(id);
            Ok(wait(job, 0))
        }
    }
}

/// `{"id": id, "pane": pane}`.
fn answer(id: Uuid, pane: StatusPane) -> ControlResult {
    ControlResult {
        pane: Some(pane.as_str().to_owned()),
        ..with_id(id)
    }
}

/// Types into a pane with no wait: a terminal not up answers `session not realized`.
fn typed_result(core: &mut Core, id: Uuid, pane: StatusPane, text: &str, reset: StatusReset) -> Outcome {
    if type_into(core, id, pane, text, reset)? {
        Ok(answer(id, pane))
    } else {
        Err(not_realized())
    }
}

/// Types `text` into pane `pane` of session `id` as user input: the keystrokes, then the status clear typing
/// carries (upstream's `injectAsUserInput`). A pane whose zmx reported a role types through its daemon's acknowledged
/// path instead (upstream's `coveredType`). False when the pane has no realized terminal.
///
/// # Errors
///
/// The daemon refused the input, or the pane may not be driven from here.
fn type_into(
    core: &mut Core,
    id: Uuid,
    pane: StatusPane,
    text: &str,
    reset: StatusReset,
) -> Result<bool, ControlError> {
    let source = {
        let core: &Core = core;
        let surface = core
            .library()
            .store_for_session(id)
            .and_then(|store| store.session(id))
            .and_then(|session| session.pane_slot(pane).get())
            .filter(|surface| surface.is_realized());
        surface.map(|surface| core.pane_source(surface, false))
    };
    let typed = match source {
        Some(PaneSource::Refused(reason)) => return Err(ControlError::rejected(reason)),
        Some(PaneSource::Daemon(name)) => {
            type_through_daemon(core, &name, text)?;
            true
        }
        Some(PaneSource::Surface) | None => core
            .library_mut()
            .store_for_session_mut(id)
            .and_then(|store| store.session_mut(id))
            .and_then(|session| session.pane_slot_mut(pane).get_mut())
            .is_some_and(|surface| surface.type_text(text)),
    };
    if typed && let Some(store) = core.library_mut().store_for_session_mut(id) {
        store.note_typed_input(id, pane, text, reset);
    }
    Ok(typed)
}

/// Types `text` into daemon `name`: the head at once, the final Return of a submitted line after
/// [`SUBMIT_GAP`](crate::keystrokes::SUBMIT_GAP). A failed final Return does not prove it was dropped: the daemon queues
/// before it answers.
fn type_through_daemon(core: &Core, name: &str, text: &str) -> Result<(), ControlError> {
    let refused = || ControlError::rejected("the pane's live session did not accept the input");
    let host = core.live.host().ok_or_else(refused)?;
    let paced = keystrokes::paced(text);
    let bytes: Vec<u8> = paced
        .head
        .into_iter()
        .flat_map(|segment| match segment {
            KeystrokeSegment::Text(run) => run.into_bytes(),
            KeystrokeSegment::Return => vec![0x0D],
        })
        .collect();
    if !bytes.is_empty() {
        host.type_input(name, &bytes).map_err(|_| refused())?;
    }
    if paced.paced_return {
        std::thread::sleep(keystrokes::SUBMIT_GAP);
        host.type_input(name, &[0x0D]).map_err(|_| {
            ControlError::rejected("text typed, but its final Return could not be confirmed; do not retype the text")
        })?;
    }
    Ok(())
}

/// Probes again after [`POLL_INTERVAL`].
fn wait(job: TypeJob, attempt: u32) -> Dispatched {
    Dispatched::Pending {
        after: POLL_INTERVAL,
        resume: Box::new(move |core: &mut Core| probe(core, job, attempt)),
    }
}

fn probe(core: &mut Core, job: TypeJob, attempt: u32) -> Dispatched {
    match try_type(core, &job) {
        Some(outcome) => Dispatched::done(outcome),
        None if attempt + 1 < ATTEMPTS => wait(job, attempt + 1),
        None => Dispatched::done(Err(not_realized())),
    }
}

/// One probe: the outcome once the pane typed or its token went away, `None` to keep waiting (a session gone
/// meanwhile included, as upstream's poll keeps probing an empty slot).
fn try_type(core: &mut Core, job: &TypeJob) -> Option<Outcome> {
    let store = core.library_mut().store_for_session_mut(job.session)?;
    let session = store.session(job.session)?;
    let pane = match &job.token {
        Some(token) => match session.pane_role_for_token(token) {
            Some(pane) => pane,
            None => return Some(Err(unknown_pane_id(token))),
        },
        None => StatusPane::Left,
    };
    match type_into(core, job.session, pane, &job.text, job.reset) {
        Ok(typed) => typed.then(|| Ok(answer(job.session, pane))),
        Err(error) => Some(Err(error)),
    }
}
