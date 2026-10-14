//! `ask.open`, `ask.result` and `ask.cancel`: validation and its messages as upstream's `ControlDispatcher+Ask.swift`,
//! the effects as `agterm/Control/ControlServer+Ask.swift`.
//!
//! A terminal ask waits on its session, shown or not; a GUI ask holds its window's modal slot and, anchored to a
//! session, needs that session on screen. The dialog itself is the frontend's: it answers through
//! [`Core::answer_session_ask`] and [`Core::answer_window_ask`].

use std::collections::HashSet;

use agterm_proto::command::AskCmd;
use agterm_proto::{Args, AskButton, ControlAskOutcome, ControlAskResult, ControlResult, Request};
use uuid::Uuid;

use super::hud::resolve_pane_placement;
use super::{Outcome, args_of};
use crate::Core;
use crate::error::ControlError;
use crate::modal::AskOwner;
use crate::model::{AskAlignment, AskAnchor, AskStyle, MAX_ASK_BUTTONS, OverlayPane, PendingAsk};
use crate::pane_env::uuid_string;
use crate::remote::AskPlacement;
use crate::resolve::Resolver;
use crate::windows::WindowRequest;

/// The placement refusal of an ask over a scratch pane token.
const INVALID_PANE: &str = "ask pane must be left or right";

/// Answers an `ask.*` command.
pub(crate) fn dispatch(core: &mut Core, cmd: AskCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let window = args.window.as_deref();
    core.sync_modals();
    match cmd {
        AskCmd::Open => open(core, request.target.as_deref(), args),
        AskCmd::Result => {
            let id = request
                .target
                .as_deref()
                .ok_or_else(|| ControlError::rejected("ask.result requires an ask id"))?;
            let result = ask_result(core, id, window)?;
            Ok(ControlResult {
                ask: Some(result),
                ..ControlResult::default()
            })
        }
        AskCmd::Cancel => {
            let id = request
                .target
                .as_deref()
                .ok_or_else(|| ControlError::rejected("ask.cancel requires an ask id"))?;
            cancel(core, id, window)?;
            Ok(ControlResult::default())
        }
    }
}

// MARK: - validation

/// `args` checked as upstream's `dispatchAskOpen`, in its order, into the dialog to open and its pane selector.
fn validated(target: Option<&str>, args: &Args) -> Result<(PendingAsk, Option<OverlayPane>), ControlError> {
    let title = args
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .ok_or_else(|| ControlError::rejected("ask.open requires a title"))?;
    let buttons = validated_buttons(args.buttons.as_deref(), title, args.message.as_deref())?;
    let ids: HashSet<&str> = buttons.iter().map(|button| button.id.as_str()).collect();
    for (role, id) in [
        ("default", &args.default_button),
        ("destructive", &args.destructive_button),
    ] {
        if let Some(id) = id
            && !ids.contains(id.as_str())
        {
            return Err(ControlError::rejected(format!("unknown {role} button: {id}")));
        }
    }
    if args.destructive_button.is_some() && args.default_button == args.destructive_button {
        return Err(ControlError::rejected("default button must not be destructive"));
    }
    let style = AskStyle::from_wire(args.style.as_deref().unwrap_or("terminal"))
        .ok_or_else(|| ControlError::rejected("unknown style"))?;
    let align = AskAlignment::from_wire(args.align.as_deref().unwrap_or("right"))
        .ok_or_else(|| ControlError::rejected("unknown align"))?;
    if args.width.is_some_and(|width| !(10..=100).contains(&width)) {
        return Err(ControlError::rejected("width must be 10 to 100"));
    }
    validate_hotkeys(&buttons)?;
    if style == AskStyle::Gui && (args.pane.is_some() || args.pane_id.is_some()) && target.is_none() {
        return Err(ControlError::rejected("--pane requires a session"));
    }
    let pane = args
        .pane
        .as_deref()
        .map(|raw| {
            OverlayPane::from_control_name(raw).ok_or_else(|| ControlError::rejected("--pane must be left or right"))
        })
        .transpose()?;
    let ask = PendingAsk {
        id: uuid_string(Uuid::new_v4()),
        title: title.to_owned(),
        message: args.message.clone(),
        buttons: buttons
            .into_iter()
            .map(|button| AskButton {
                hotkey: button.hotkey.map(|key| key.to_lowercase()),
                ..button
            })
            .collect(),
        default_id: args.default_button.clone(),
        destructive_id: args.destructive_button.clone(),
        style,
        align,
        width: args.width,
        anchor: None,
    };
    Ok((ask, pane))
}

/// The button list's own checks, and the control-character check over every text the dialog shows.
fn validated_buttons(
    buttons: Option<&[AskButton]>,
    title: &str,
    message: Option<&str>,
) -> Result<Vec<AskButton>, ControlError> {
    let buttons = buttons.ok_or_else(|| ControlError::rejected("ask.open requires buttons"))?;
    if buttons.is_empty() {
        return Err(ControlError::rejected("ask.open requires at least one button"));
    }
    if buttons.len() > MAX_ASK_BUTTONS {
        return Err(ControlError::rejected(format!(
            "too many buttons (max {MAX_ASK_BUTTONS})"
        )));
    }
    if buttons.iter().any(|button| button.label.is_empty()) {
        return Err(ControlError::rejected("ask button label must not be empty"));
    }
    let mut ids = HashSet::new();
    if !buttons.iter().all(|button| ids.insert(button.id.as_str())) {
        return Err(ControlError::rejected("ask button ids must be unique"));
    }
    if contains_control_characters(title)
        || contains_control_characters(message.unwrap_or_default())
        || buttons.iter().any(|button| contains_control_characters(&button.label))
    {
        return Err(ControlError::rejected("ask text must not contain control characters"));
    }
    Ok(buttons.to_vec())
}

/// Each hotkey is one ASCII letter, unique case-insensitively.
fn validate_hotkeys(buttons: &[AskButton]) -> Result<(), ControlError> {
    let mut seen = HashSet::new();
    for hotkey in buttons.iter().filter_map(|button| button.hotkey.as_deref()) {
        if hotkey.len() != 1 || !hotkey.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(ControlError::rejected("ask button hotkey must be one ASCII letter"));
        }
        if !seen.insert(hotkey.to_ascii_lowercase()) {
            return Err(ControlError::rejected("ask button hotkeys must be unique"));
        }
    }
    Ok(())
}

/// Whether `text` carries a C0 control character or DEL (upstream's `containsControlCharacters`).
pub(crate) fn contains_control_characters(text: &str) -> bool {
    text.chars().any(|ch| u32::from(ch) < 0x20 || u32::from(ch) == 0x7f)
}

// MARK: - open

/// Opens a validated ask: a terminal one on its session, a GUI one in its window's slot.
fn open(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let (ask, pane) = validated(target, args)?;
    let placement = Placement {
        pane,
        pane_id: args.pane_id.as_deref(),
        follow: args.follow == Some(true),
    };
    let window = args.window.as_deref();
    let opened = if ask.style == AskStyle::Terminal {
        let resolved = Resolver::new(core.library()).session(target, window)?;
        open_terminal(core, ask, resolved.window, resolved.session, &placement)
    } else if target.is_some() {
        let resolved = Resolver::new(core.library()).session(target, window)?;
        open_gui(core, ask, resolved.window, Some(resolved.session), &placement)
    } else {
        let window = Resolver::new(core.library()).open_placement_window(window)?;
        open_gui(core, ask, window, None, &placement)
    };
    core.sync_modals();
    opened
}

/// Where the caller asked the dialog to sit, and whether to raise its window.
struct Placement<'a> {
    pane: Option<OverlayPane>,
    pane_id: Option<&'a str>,
    follow: bool,
}

/// Hands the ask to the viewer presenting `session`, or `None` to open it here.
fn present_remotely(
    core: &mut Core,
    ask: &PendingAsk,
    window: Uuid,
    session: Uuid,
    placement: &Placement<'_>,
) -> Option<Outcome> {
    let remote = AskPlacement {
        pane: placement.pane,
        pane_id: placement.pane_id,
        invalid_pane: INVALID_PANE,
    };
    crate::remote::present_ask_remotely(core, ask, window, session, &remote)
}

/// Upstream's `presentTerminalAsk`: the pane must be laid out, the session's slot free.
fn open_terminal(core: &mut Core, ask: PendingAsk, window: Uuid, session: Uuid, placement: &Placement<'_>) -> Outcome {
    if let Some(outcome) = present_remotely(core, &ask, window, session, placement) {
        return outcome;
    }
    let id = ask.id.clone();
    let store = core.library_mut().store_mut(window).ok_or_else(no_such_session)?;
    let target = store.session(session).ok_or_else(no_such_session)?;
    let resolved = resolve_pane_placement(target, placement.pane, placement.pane_id, true, INVALID_PANE)?;
    if !store.open_session_ask(session, ask, resolved.map(|(identity, _)| identity)) {
        return Err(ControlError::rejected("ask already pending"));
    }
    core.modals
        .register_ask(id.clone(), AskOwner::Session { session, window });
    follow(core, window, placement.follow);
    Ok(opened(id, resolved.map(|(_, pane)| pane)))
}

/// Upstream's `presentAsk`: a session anchor must be on screen (selected, no zoom, no dashboard), the window's slot
/// free.
fn open_gui(
    core: &mut Core,
    mut ask: PendingAsk,
    window: Uuid,
    session: Option<Uuid>,
    placement: &Placement<'_>,
) -> Outcome {
    if let Some(session) = session {
        if let Some(outcome) = present_remotely(core, &ask, window, session, placement) {
            return outcome;
        }
        let store = core.library().store(window).ok_or_else(no_such_session)?;
        let target = store.session(session).ok_or_else(no_such_session)?;
        if store.selected_session_id() != Some(session)
            || core.zoom_target(window).is_some()
            || core.dashboard(window).is_some()
        {
            return Err(ControlError::rejected("session not visible"));
        }
        let resolved = resolve_pane_placement(target, placement.pane, placement.pane_id, true, INVALID_PANE)?;
        ask.anchor = Some(AskAnchor {
            session,
            pane: resolved.map(|(_, pane)| pane),
            pane_identity: resolved.map(|(identity, _)| identity),
        });
    }
    let id = ask.id.clone();
    let pane = ask.anchor.as_ref().and_then(|anchor| anchor.pane);
    core.modals
        .open_window_ask(window, ask)
        .map_err(ControlError::rejected)?;
    follow(core, window, placement.follow);
    Ok(opened(id, pane))
}

/// `--follow`: raise the owning window and make it frontmost.
fn follow(core: &mut Core, window: Uuid, follow: bool) {
    if follow {
        core.request_window(WindowRequest::Raise(window));
        core.take_frontmost(window);
    }
}

fn opened(id: String, pane: Option<OverlayPane>) -> ControlResult {
    ControlResult {
        id: Some(id),
        pane: pane.map(|pane| pane.wire_name().to_owned()),
        ..ControlResult::default()
    }
}

fn no_such_session() -> ControlError {
    ControlError::rejected("no such session")
}

// MARK: - result and cancel

fn unknown_ask(id: &str) -> ControlError {
    ControlError::rejected(format!("unknown ask: {id}"))
}

/// The pending or retained outcome of ask `id`, scoped to the `--window` window when one is named (upstream's
/// `withAskResult`).
fn ask_result(core: &Core, id: &str, window: Option<&str>) -> Result<ControlAskResult, ControlError> {
    let (result, owner_window) = lookup(core, id).ok_or_else(|| unknown_ask(id))?;
    if window.is_some() && Resolver::new(core.library()).window_id(window)? != owner_window {
        return Err(unknown_ask(id));
    }
    Ok(result)
}

/// Ask `id`'s current outcome and its window: pending while its owner still holds it, else the retained outcome
/// (upstream's `AskRegistry.result`). A registered id its owner no longer holds is unknown.
fn lookup(core: &Core, id: &str) -> Option<(ControlAskResult, Uuid)> {
    let Some(owner) = core.modals.ask_owner(id) else {
        return core.modals.finished_ask(id);
    };
    let held = match owner {
        AskOwner::Window(window) => core.modals.window_ask(window).is_some_and(|ask| ask.id == id),
        AskOwner::Session { session, window } => core
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .and_then(|session| session.ask())
            .is_some_and(|pending| pending.ask.id == id),
    };
    held.then(|| (ControlAskResult::outcome(ControlAskOutcome::Pending), owner.window()))
}

/// Cancels ask `id` while it is pending; a finished one is a no-op that keeps its outcome.
fn cancel(core: &mut Core, id: &str, window: Option<&str>) -> Result<(), ControlError> {
    let result = ask_result(core, id, window)?;
    if result.result != ControlAskOutcome::Pending {
        return Ok(());
    }
    let cancelled = ControlAskResult::outcome(ControlAskOutcome::Cancelled);
    match core.modals.ask_owner(id) {
        Some(AskOwner::Window(window)) => {
            if core.modals.window_ask(window).is_some_and(|ask| ask.id == id) {
                core.modals.resolve_window_ask(window, cancelled);
            }
        }
        Some(AskOwner::Session { session, window }) => {
            if let Some(store) = core.library_mut().store_mut(window) {
                store.resolve_session_ask(session, id, cancelled);
            }
        }
        None => return Err(unknown_ask(id)),
    }
    core.sync_modals();
    Ok(())
}
