//! `workspace.*`: `new`, `select`, `go`, `rename`, `delete`, `move`, `focus`, `filter`, `collapse` and `expand`.
//! Validation and its messages follow `dispatchWorkspaceCommand` in upstream's `ControlDispatcher.swift`; the effects
//! follow `agterm/Control/ControlServer+WorkspaceCommands.swift` over the store's `AppStore.swift` and
//! `AppStore+Focus.swift` mutators.

use ngterm_proto::command::WorkspaceCmd;
use ngterm_proto::{Args, ControlResult, Request};

use super::{ToggleMode, args_of, in_placement_store, with_id, workspace_store};
use crate::Core;
use crate::error::ControlError;
use crate::model::naming::trimmed_or_none;
use crate::model::{FocusMode, ReorderDirection, WorkspaceNavigation, WorkspaceOptions};
use crate::resolve::Resolver;

/// What a workspace command answers: a result object, or `None` for a bare `{"ok": true}` (`workspace.filter`).
pub(crate) type WorkspaceOutcome = Result<Option<ControlResult>, ControlError>;

/// Answers a `workspace.*` command. A targeted command checks its own arguments before its target resolves, as
/// upstream's dispatcher does, so a bad argument never touches the model.
pub(crate) fn dispatch(core: &mut Core, cmd: WorkspaceCmd, request: &Request) -> WorkspaceOutcome {
    let args = args_of(request);
    let action = match cmd {
        WorkspaceCmd::Filter => return filter(core, args).map(|()| None),
        WorkspaceCmd::New => return new(core, args).map(Some),
        WorkspaceCmd::Go => return go(core, args).map(Some),
        WorkspaceCmd::Select => WorkspaceAction::Select,
        WorkspaceCmd::Rename => WorkspaceAction::rename(args)?,
        WorkspaceCmd::Delete => WorkspaceAction::Delete,
        WorkspaceCmd::Move => WorkspaceAction::reorder(args)?,
        WorkspaceCmd::Focus => WorkspaceAction::focus(args)?,
        WorkspaceCmd::Collapse => WorkspaceAction::Expand(false),
        WorkspaceCmd::Expand => WorkspaceAction::Expand(true),
    };
    apply(core, action, request.target.as_deref(), args.window.as_deref()).map(Some)
}

/// A command acting on one workspace target, its arguments checked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceAction {
    Select,
    Rename(String),
    Delete,
    Move(ReorderDirection),
    Focus(FocusMode),
    /// `collapse` (false) or `expand` (true).
    Expand(bool),
}

impl WorkspaceAction {
    /// `workspace.rename`: the trimmed `--name`, required.
    fn rename(args: &Args) -> Result<Self, ControlError> {
        args.name
            .as_deref()
            .and_then(trimmed_or_none)
            .map(Self::Rename)
            .ok_or_else(|| ControlError::rejected("workspace.rename requires a name"))
    }

    /// `workspace.move`: `--to up|down|top|bottom`, required.
    fn reorder(args: &Args) -> Result<Self, ControlError> {
        let to = args
            .to
            .as_deref()
            .ok_or_else(|| ControlError::rejected("workspace.move requires --to"))?;
        ReorderDirection::from_wire(to)
            .map(Self::Move)
            .ok_or_else(|| ControlError::rejected("workspace.move --to must be up|down|top|bottom"))
    }

    /// `workspace.focus`: the mode, `toggle` when absent.
    fn focus(args: &Args) -> Result<Self, ControlError> {
        let raw = args.mode.as_deref().unwrap_or(FocusMode::Toggle.wire_name());
        FocusMode::from_wire(raw).map(Self::Focus).ok_or_else(|| {
            ControlError::rejected(format!("invalid focus mode: {raw} ({})", FocusMode::valid_names_list()))
        })
    }
}

/// Resolves the workspace target (default `active`, the current workspace) and applies `action` to it.
fn apply(
    core: &mut Core,
    action: WorkspaceAction,
    target: Option<&str>,
    window: Option<&str>,
) -> Result<ControlResult, ControlError> {
    let (store, id) = workspace_store(core, target, window)?;
    match action {
        WorkspaceAction::Select => {
            store.select_workspace(id);
        }
        WorkspaceAction::Rename(name) => store.rename_workspace(id, &name),
        WorkspaceAction::Delete => {
            // the GUI asks before deleting; the control channel refuses the last workspace instead
            if !store.can_remove_workspace() {
                return Err(ControlError::rejected("cannot delete last workspace"));
            }
            store.remove_workspace(id);
        }
        WorkspaceAction::Move(direction) => store.reorder_workspace(id, direction),
        WorkspaceAction::Focus(mode) => store.apply_focus_mode(mode, id),
        WorkspaceAction::Expand(expanded) => store.set_workspace_expanded_in_outline(id, expanded),
    }
    Ok(with_id(id))
}

/// `workspace.new`: appends a workspace named `--name` (trimmed; blank or absent takes the next default name) to the
/// `--window` (else frontmost) window. A plain create becomes current and joins an enabled focus filter, as the GUI's
/// New Workspace button; `--collapsed` seeds it folded and creates it in the background: it neither takes the current
/// workspace nor widens the marked set, so a script can fill it with `session.new --no-select` unseen.
fn new(core: &mut Core, args: &Args) -> Result<ControlResult, ControlError> {
    let collapsed = args.collapsed == Some(true);
    in_placement_store(core, args.window.as_deref(), |store| {
        let name = args
            .name
            .as_deref()
            .and_then(trimmed_or_none)
            .unwrap_or_else(|| store.default_workspace_name());
        let id = store.add_workspace_with(
            &name,
            WorkspaceOptions {
                collapsed,
                reveal: !collapsed,
            },
        );
        Ok(with_id(id))
    })
}

/// `workspace.go`: steps the current workspace of the `--window` (else frontmost) window one place through the
/// sidebar's visible order, wrapping, and selects the destination's first session when it has one.
fn go(core: &mut Core, args: &Args) -> Result<ControlResult, ControlError> {
    let direction = args
        .to
        .as_deref()
        .and_then(WorkspaceNavigation::from_wire)
        .ok_or_else(|| ControlError::rejected("workspace.go requires --to next|prev"))?;
    in_placement_store(core, args.window.as_deref(), |store| {
        store
            .navigate_workspace(direction)
            .map(|step| with_id(step.workspace))
            .ok_or_else(|| ControlError::rejected("no other workspace to navigate to"))
    })
}

/// `workspace.filter`: turns the window's focus filter on, off or over, leaving the marked set alone. Window-scoped,
/// so it fails with no window open rather than acting on nothing; enabling an empty set succeeds having changed
/// nothing.
fn filter(core: &mut Core, args: &Args) -> Result<(), ControlError> {
    let mode = ToggleMode::parse(args.mode.as_deref()).ok_or_else(|| {
        ControlError::rejected(format!(
            "invalid workspace filter mode: {}",
            args.mode.as_deref().unwrap_or("toggle")
        ))
    })?;
    let window = Resolver::new(core.library()).open_placement_window(args.window.as_deref())?;
    let store = core.library_mut().store_mut(window).ok_or(ControlError::NoOpenWindow)?;
    let enabled = mode.desired_value(store.focus().enabled);
    store.set_focus_enabled(enabled);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(edit: impl FnOnce(&mut Args)) -> Args {
        let mut args = Args::default();
        edit(&mut args);
        args
    }

    #[test]
    fn targeted_arguments_parse_as_upstreams() {
        let some = |value: &str| Some(value.to_owned());
        assert_eq!(
            WorkspaceAction::rename(&args(|a| a.name = some("  build  "))),
            Ok(WorkspaceAction::Rename("build".to_owned()))
        );
        assert_eq!(
            WorkspaceAction::reorder(&args(|a| a.to = some("bottom"))),
            Ok(WorkspaceAction::Move(ReorderDirection::Bottom))
        );
        assert_eq!(
            WorkspaceAction::focus(&Args::default()),
            Ok(WorkspaceAction::Focus(FocusMode::Toggle))
        );
        for mode in FocusMode::ALL {
            assert_eq!(
                WorkspaceAction::focus(&args(|a| a.mode = some(mode.wire_name()))),
                Ok(WorkspaceAction::Focus(mode))
            );
        }
    }
}
