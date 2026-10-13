//! `pick.open`, `pick.result` and `pick.cancel`: validation and its messages as upstream's
//! `ControlDispatcher+Pick.swift`, the effects as `agterm/Control/ControlServer+Pick.swift`. The picker holds its
//! window's modal slot; the frontend draws it and answers through [`Core::resolve_pick`].

use std::collections::HashSet;

use agterm_proto::command::PickCmd;
use agterm_proto::{ControlPickOutcome, ControlPickResult, ControlResult, PickItem, Request};
use uuid::Uuid;

use super::ask::contains_control_characters;
use super::{Outcome, args_of};
use crate::Core;
use crate::error::ControlError;
use crate::modal::{MAX_PICK_ITEMS, PendingPick};
use crate::pane_env::uuid_string;
use crate::resolve::Resolver;
use crate::windows::WindowRequest;

/// Answers a `pick.*` command.
pub(crate) fn dispatch(core: &mut Core, cmd: PickCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let window = args.window.as_deref();
    core.sync_modals();
    match cmd {
        PickCmd::Open => {
            let pick = validated(request)?;
            let id = pick.id.clone();
            let owner = placement_window(core, window)?;
            core.modals.open_pick(owner, pick).map_err(ControlError::rejected)?;
            if args.follow == Some(true) {
                core.request_window(WindowRequest::Raise(owner));
                core.take_frontmost(owner);
            }
            core.sync_modals();
            Ok(ControlResult::id(id))
        }
        PickCmd::Result => {
            let id = request
                .target
                .as_deref()
                .ok_or_else(|| ControlError::rejected("pick.result requires a pick id"))?;
            let result = pick_result(core, id, window)?;
            Ok(ControlResult {
                pick: Some(result),
                ..ControlResult::default()
            })
        }
        PickCmd::Cancel => {
            let id = request
                .target
                .as_deref()
                .ok_or_else(|| ControlError::rejected("pick.cancel requires a pick id"))?;
            cancel(core, id, window)?;
            Ok(ControlResult::default())
        }
    }
}

/// The picker `request` asks for, checked as upstream's `dispatchPickCommand`, in its order.
fn validated(request: &Request) -> Result<PendingPick, ControlError> {
    let args = args_of(request);
    let items = args
        .items
        .as_deref()
        .ok_or_else(|| ControlError::rejected("pick.open requires items"))?;
    let allow_custom = args.allow_custom == Some(true);
    // an empty list is a text prompt, which only makes sense when a custom answer is accepted
    if items.is_empty() && !allow_custom {
        return Err(ControlError::rejected("pick.open requires at least one item"));
    }
    if items.len() > MAX_PICK_ITEMS {
        return Err(ControlError::rejected(format!("too many items (max {MAX_PICK_ITEMS})")));
    }
    if items.iter().any(|item| item.label.is_empty()) {
        return Err(ControlError::rejected("pick item label must not be empty"));
    }
    let mut ids = HashSet::new();
    if !items.iter().all(|item| ids.insert(item.id.as_str())) {
        return Err(ControlError::rejected("pick item ids must be unique"));
    }
    if items.iter().any(has_control_characters) {
        return Err(ControlError::rejected("item text must not contain control characters"));
    }
    // checked against the caller's items, not the rows a query prefill leaves visible
    if let Some(selection) = &args.selection
        && !ids.contains(selection.as_str())
    {
        return Err(ControlError::rejected("pick select must name an item id"));
    }
    Ok(PendingPick {
        id: uuid_string(Uuid::new_v4()),
        items: items.to_vec(),
        prompt: args.prompt.clone(),
        query: args.query.clone(),
        allow_custom,
        selection: args.selection.clone(),
    })
}

fn has_control_characters(item: &PickItem) -> bool {
    contains_control_characters(&item.label) || item.subtitle.as_deref().is_some_and(contains_control_characters)
}

/// The window a picker command acts on (upstream's `withPickController`): the `--window` one, else the frontmost;
/// refused when no window is open.
fn placement_window(core: &Core, window: Option<&str>) -> Result<Uuid, ControlError> {
    if core.library().active_window_id().is_none() {
        return Err(ControlError::NoOpenWindow);
    }
    Resolver::new(core.library())
        .placement_window(window)?
        .ok_or(ControlError::NoOpenWindow)
}

fn unknown_pick(id: &str) -> ControlError {
    ControlError::rejected(format!("unknown pick: {id}"))
}

/// Picker `id`'s current or retained result (upstream's `pickResult`). Without `--window` the globally unique id finds
/// it in any window, so a later frontmost change cannot orphan a blocking client; with one, the result must belong to
/// that window.
fn pick_result(core: &Core, id: &str, window: Option<&str>) -> Result<ControlPickResult, ControlError> {
    if window.is_none() {
        return core
            .modals
            .live_pick(id)
            .or_else(|| core.modals.retained_pick(id))
            .map(|(_, result)| result)
            .ok_or_else(|| unknown_pick(id));
    }
    if let Some((owner, result)) = core.modals.retained_pick(id) {
        return if Resolver::new(core.library()).window_id(window)? == owner {
            Ok(result)
        } else {
            Err(unknown_pick(id))
        };
    }
    let owner = placement_window(core, window)?;
    core.modals
        .controller(owner)
        .and_then(|controller| controller.result(id))
        .ok_or_else(|| unknown_pick(id))
}

/// Cancels picker `id` while it is pending; a finished one is a no-op (upstream's `cancelPick`).
fn cancel(core: &mut Core, id: &str, window: Option<&str>) -> Result<(), ControlError> {
    let owner = if window.is_none() {
        match core.modals.live_pick(id) {
            Some((owner, _)) => owner,
            None if core.modals.retained_pick(id).is_some() => return Ok(()),
            None => return Err(unknown_pick(id)),
        }
    } else {
        if let Some((owner, _)) = core.modals.retained_pick(id) {
            return if Resolver::new(core.library()).window_id(window)? == owner {
                Ok(())
            } else {
                Err(unknown_pick(id))
            };
        }
        let owner = placement_window(core, window)?;
        if core
            .modals
            .controller(owner)
            .and_then(|controller| controller.result(id))
            .is_none()
        {
            return Err(unknown_pick(id));
        }
        owner
    };
    if core
        .modals
        .controller(owner)
        .and_then(|controller| controller.pending())
        .is_some_and(|pick| pick.id == id)
    {
        core.modals
            .resolve_pick(owner, ControlPickResult::outcome(ControlPickOutcome::Cancelled));
        core.sync_modals();
    }
    Ok(())
}
