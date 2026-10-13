//! The split and scratch commands: `session.split`, `session.split.close`, `session.swap`, `session.lead`,
//! `session.scratch`, `session.focus` and `session.resize`. Validation and its messages follow upstream's
//! `dispatchSessionSurfaceCommand` (`ControlDispatcher.swift`); the effects follow `splitSession`, `closeSessionSplit`,
//! `scratchSession`, `focusSessionPane` and `resizeSplit` (`agterm/Control/ControlServer+SessionActions.swift`),
//! `swapSessionPanes` (`AppActions.swift`) and `takeSessionLead` (`ControlServer+PaneLead.swift`).
//!
//! Keyboard focus is the frontend's: its deck gives input to the pane the model marks focused whenever that changes, so
//! these commands only change the model (upstream's `focusSplitPane` calls); `session.focus` also leaves a one-shot
//! [`Core::pane_focus_requested`] so the pane takes the keyboard back even when the model's choice did not change.
//! The frontend reports every focus it sees or gives through [`Core::pane_focused`], which withdraws the session's
//! delivered banners; a successful `session.swap` withdraws them too, without touching the badge.

use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, ControlResult, Request, Response};
use uuid::Uuid;

use super::surface::{POLL_INTERVAL, no_split_pane, not_realized};
use super::typing::ATTEMPTS;
use super::{Outcome, ToggleMode, args_of, not_found, parse_surface_pane, session_store, with_id};
use crate::Core;
use crate::dispatch::Dispatched;
use crate::error::{ControlError, Noun};
use crate::model::{SplitAxis, StatusPane, SwapRefusal};
use crate::resolve::ACTIVE;

/// A terminal's report bound for its session's pane fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneReport {
    /// An OSC 7 working directory.
    Pwd(String),
    /// A title a program set (or the user's static one).
    Title(String),
}

impl Core {
    /// The shell of the pane carrying `token` exited (upstream's per-pane `onExit`): the primary's promotes a split
    /// survivor or closes the session, the split pane's collapses the split or closes the session it was the last pane
    /// of, the scratch's closes the scratch. A token no pane of the session carries any more (the terminal was replaced
    /// or torn down meanwhile) changes nothing. Whether it changed anything; commit afterwards when it did.
    pub fn pane_exited(&mut self, session: Uuid, token: &str) -> bool {
        // a restart ended this shell on purpose and rebuilds the pane
        if self.zmx.claimed_exits.contains(token) {
            return false;
        }
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        match store
            .session(session)
            .and_then(|found| found.pane_role_for_token(token))
        {
            Some(StatusPane::Left) => store.close_primary_pane(session),
            Some(StatusPane::Right) => store.close_split_pane(session),
            Some(StatusPane::Scratch) => return store.close_scratch(session, true),
            None => return false,
        }
        true
    }

    /// Keyboard focus moved into the pane carrying `token`: the user clicked it, or the frontend gave it focus
    /// (a selection, a `session.focus`, a reveal), even to a terminal that already held it. A shown split's focused
    /// pane follows it; see [`crate::model::AppStore::note_pane_focus`]. While the session's window is zoomed into a
    /// terminal the split's focus stays put: the zoom gives its terminal the keyboard itself, and that must not move
    /// the split's focus (upstream's zoom hosts suppress focus reports). Zoomed or not, the session was seen: its unseen
    /// badge clears and its delivered banners are withdrawn (upstream's `onFocusChange` and the focus-free
    /// `onClearUnseen`, both ending in `clearDelivered`). Only the primary and split panes report, as upstream wires
    /// neither callback to the scratch; a token no pane of the session carries any more changes nothing. True whenever
    /// a withdrawal was queued, even with the badge already clear; settle afterwards.
    pub fn pane_focused(&mut self, session: Uuid, token: &str) -> bool {
        let zoomed = self
            .library()
            .window_holding_session(session)
            .is_some_and(|window| self.zoom_target(window).is_some());
        let pane = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
            .and_then(|found| found.pane_role_for_token(token));
        let Some(pane @ (StatusPane::Left | StatusPane::Right)) = pane else {
            return false;
        };
        if !self.clear_delivered(session) {
            return false;
        }
        if let Some(store) = self.library_mut().store_for_session_mut(session) {
            store.clear_unseen(session);
            if !zoomed {
                store.note_pane_focus(session, pane);
            }
        }
        true
    }

    /// Lands a pane's cwd or title report on the pane carrying `token` (upstream's `applyPwd`/`applyTitle`, routed by
    /// the pane's role now, so a swapped or promoted terminal reports into its new role's fields), wherever its session
    /// lives: in a window's tree, or held by a pending soft close whose terminal still runs. The scratch reports into
    /// nothing, as upstream's scratch is not wired to the session's fields. Nothing is saved. Whether it landed.
    pub fn report_pane(&mut self, session: Uuid, token: &str, report: &PaneReport) -> bool {
        // a live pane's zmx reports its lead as a title under a reserved prefix, never shown as one
        if let PaneReport::Title(title) = report
            && let Some(notice) = crate::zmx::lead::LeadNotice::parse(title)
        {
            return self.lead_reported(session, token, &notice);
        }
        let Some(target) = self.library_mut().held_session_mut(session) else {
            return false;
        };
        let split = match target.pane_role_for_token(token) {
            Some(StatusPane::Left) => false,
            Some(StatusPane::Right) => true,
            Some(StatusPane::Scratch) | None => return false,
        };
        match report {
            PaneReport::Pwd(pwd) => target.report_pwd(split, pwd),
            PaneReport::Title(title) => target.report_title(split, title),
        }
        true
    }
}

/// Answers a split or scratch command of the session group that answers at once.
pub(crate) fn dispatch(core: &mut Core, cmd: SessionCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let target = request.target.as_deref();
    match cmd {
        SessionCmd::Split => split(core, target, args),
        SessionCmd::SplitClose => close_split(core, target, args.window.as_deref()),
        SessionCmd::Lead => lead(core, target, args),
        SessionCmd::Scratch => scratch(core, target, args),
        SessionCmd::Focus => focus(core, target, args),
        SessionCmd::Resize => resize(core, target, args),
        // `session.swap` starts through `deferred`
        other => Err(ControlError::NotSupported(Command::Session(other))),
    }
}

fn no_split() -> ControlError {
    ControlError::rejected("session has no split")
}

// MARK: - split

/// `session.split [on|off|toggle] [--axis]`: shows, hides or toggles the split against its shown state; a hidden pane
/// keeps its shell. An explicit axis creates or transposes, none keeps the current one.
fn split(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let axis = args
        .axis
        .as_deref()
        .map(|raw| {
            SplitAxis::from_wire(raw)
                .ok_or_else(|| ControlError::rejected(format!("invalid split axis: {raw} (vertical|horizontal)")))
        })
        .transpose()?;
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let mode = ToggleMode::parse(args.mode.as_deref()).ok_or_else(|| {
        ControlError::rejected(format!(
            "invalid split mode: {}",
            args.mode.as_deref().unwrap_or("toggle")
        ))
    })?;
    match mode {
        ToggleMode::On => store.set_split_visibility(id, true, axis),
        ToggleMode::Off => store.set_split_visibility(id, false, None),
        ToggleMode::Toggle => store.toggle_split(id, axis),
    }
    Ok(with_id(id))
}

/// `session.split.close`: tears the split pane down, killing what it runs; ok without one.
fn close_split(core: &mut Core, target: Option<&str>, window: Option<&str>) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    if store.session(id).is_some_and(|session| session.split.exists) {
        store.close_split(id);
    }
    Ok(with_id(id))
}

// MARK: - swap

/// Starts `session.swap`: exchanges the two panes, waiting up to [`ATTEMPTS`] probes [`POLL_INTERVAL`] apart while a
/// split requested a moment ago has no terminal in both slots yet.
pub(crate) fn begin_swap(core: &mut Core, request: &Request) -> Dispatched {
    let args = args_of(request);
    match session_store(core, request.target.as_deref(), args.window.as_deref()) {
        Ok((_, id)) => swap_step(core, id, 0),
        Err(error) => Dispatched::Done(Response::from(error)),
    }
}

fn swap_step(core: &mut Core, id: Uuid, attempt: u32) -> Dispatched {
    let refusal = match core.library_mut().store_for_session_mut(id) {
        Some(store) => store.swap_panes(id).err(),
        None => Some(SwapRefusal::NoSession),
    };
    match refusal {
        None => {
            // the terminals traded slots, so a delivered banner's pane identity names the other one now: withdraw them
            // (upstream's swap handler calls `clearDelivered`); the badge stays
            core.clear_delivered(id);
            Dispatched::done(Ok(with_id(id)))
        }
        Some(SwapRefusal::SlotNotRealized) if attempt < ATTEMPTS => Dispatched::Pending {
            after: POLL_INTERVAL,
            resume: Box::new(move |core: &mut Core| swap_step(core, id, attempt + 1)),
        },
        Some(refusal) => Dispatched::done(Err(swap_error(refusal))),
    }
}

fn swap_error(refusal: SwapRefusal) -> ControlError {
    match refusal {
        SwapRefusal::NoSession => ControlError::rejected("session closed during swap"),
        SwapRefusal::NoSplit => no_split_pane(),
        SwapRefusal::SlotNotRealized => not_realized(),
    }
}

// MARK: - lead

/// `session.lead [--pane]`: takes the lead of a pane's live-session client. Without a live-session client that ever
/// reported a role there is no lead to take, which is every pane until live sessions arrive.
fn lead(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let pane = parse_surface_pane(args.pane.as_deref())?;
    if pane == Some(StatusPane::Scratch) {
        return Err(ControlError::rejected("the scratch terminal has no lead"));
    }
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store.session(id).ok_or_else(not_realized)?;
    let right = pane == Some(StatusPane::Right);
    if right && !session.split_surface.is_present() {
        return Err(no_split_pane());
    }
    let slot = if right {
        &session.split_surface
    } else {
        &session.surface
    };
    let Some(token) = slot
        .get()
        .map(|surface| surface.pane_token().to_owned())
        .filter(|token| Uuid::parse_str(token).is_ok())
    else {
        return Err(not_realized());
    };
    // a pane that already leads answers ok, so a caller can ask without reading `lead` first; one whose zmx never
    // reported a role has no lead to take
    let Ok(identity) = Uuid::parse_str(&token) else {
        return Err(not_realized());
    };
    let leads = &core.zmx.leads;
    let covered = leads.covered(identity);
    if leads.role(identity).is_none() && !covered {
        return Err(ControlError::rejected("pane has no lead to take"));
    }
    if covered && !leads.reattaching(identity) {
        core.reattach_pane(id, &token, true);
    }
    Ok(with_id(id))
}

// MARK: - scratch

/// `session.scratch [on|off|toggle] [--command]`: shows, hides or toggles the scratch terminal; hiding keeps its
/// shell. A command (when showing) runs once instead of a login shell, respawning a live scratch with it. Showing
/// selects the session first, as the scratch takes the keyboard.
fn scratch(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let mode = ToggleMode::parse(args.mode.as_deref()).ok_or_else(|| {
        ControlError::rejected(format!(
            "invalid scratch mode: {}",
            args.mode.as_deref().unwrap_or("toggle")
        ))
    })?;
    let Some(session) = store.session(id) else {
        return Ok(with_id(id));
    };
    let active = session.scratch_active;
    let want = mode.desired_value(active);
    let command = args.command.as_deref().filter(|command| !command.is_empty());
    // replacing a shown scratch's program ends shown as it started, so neither half emits `pane.scratch`
    let respawning_visible = want && active && session.scratch_surface.is_present() && command.is_some();
    if want && let Some(command) = command {
        // the close hides it, so the toggle below shows it again with the new command
        store.close_scratch(id, !respawning_visible);
        if let Some(session) = store.session_mut(id) {
            session.scratch_command = Some(command.to_owned());
        }
    }
    if want && store.selected_session_id() != Some(id) {
        store.select_session(Some(id));
    }
    if store.session(id).is_some_and(|session| session.scratch_active != want) {
        store.toggle_scratch(id, !respawning_visible);
    }
    Ok(with_id(id))
}

// MARK: - focus and resize

/// `session.focus [left|right|other]` (and the role and position aliases): moves the split's focus, shown or hidden.
fn focus(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let split = store.session(id).map(|session| session.split);
    let Some(split) = split.filter(|split| split.exists) else {
        return Err(no_split());
    };
    let raw = args.pane.as_deref().unwrap_or("other");
    let to_split = match raw {
        "left" | "top" | "primary" => false,
        "right" | "bottom" | "split" => true,
        "other" | "toggle" => !split.focused,
        _ => return Err(ControlError::rejected(format!("invalid pane: {raw}"))),
    };
    store.set_split_focus(id, to_split);
    // the pane takes the keyboard even when the model already focused it, wherever focus sits now
    core.request_pane_focus(id);
    Ok(with_id(id))
}

/// `session.resize`: sets the divider to an absolute primary-pane fraction or nudges it by a signed delta (from an even
/// split when never moved), clamped; works hidden, applying when shown. Echoes the applied fraction.
fn resize(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let resize = match (args.ratio, args.ratio_delta) {
        (None, None) => {
            return Err(ControlError::rejected(
                "session.resize requires --split-ratio, --grow-left, or --grow-right",
            ));
        }
        (Some(_), Some(_)) => {
            return Err(ControlError::rejected(
                "session.resize: --split-ratio is mutually exclusive with --grow-left/--grow-right",
            ));
        }
        (Some(ratio), None) => Resize::Ratio(ratio),
        (None, Some(delta)) => Resize::Delta(delta),
    };
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let split = store.session(id).map(|session| session.split);
    let Some(split) = split.filter(|split| split.exists) else {
        return Err(no_split());
    };
    let requested = match resize {
        Resize::Ratio(ratio) => ratio,
        Resize::Delta(delta) => split.ratio.unwrap_or(crate::model::pane::SPLIT_RATIO_DEFAULT) + delta,
    };
    let applied = store
        .apply_split_ratio(requested, id)
        .ok_or_else(|| not_found(Noun::Session, target.unwrap_or(ACTIVE)))?;
    Ok(ControlResult {
        ratio: Some(applied),
        ..with_id(id)
    })
}

#[derive(Debug, Clone, Copy)]
enum Resize {
    Ratio(f64),
    Delta(f64),
}
