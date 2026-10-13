//! `session.*` lifecycle and order: `new`, `duplicate`, `close`, `select`, `go`, `rename`, `reveal` and `move`.
//! Validation and its messages follow `dispatchSessionCommand`/`dispatchSessionMove` in upstream's
//! `ControlDispatcher.swift`; the effects follow `agterm/Control/ControlServer+SessionActions.swift`
//! (`createSession`, `duplicateSession`, `closeSession(s)`, `moveSession(s)`, `placeSession(s)`, ...).
//!
//! Upstream focuses the active session after a foreground create or duplicate in the frontmost window
//! (`focusActiveSession`); here the frontend's deck focuses the selected session whenever it settles, so the core does
//! nothing for it.

use std::path::{Path, PathBuf};

use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, Request};
use uuid::Uuid;

use super::{
    Outcome, args_of, in_placement_store, session_batch_store, session_in, session_store, with_affected, with_id,
};
use crate::Core;
use crate::error::{ControlError, Noun};
use crate::model::{AppStore, NewSession, ReorderDirection, SessionLocation, SessionNavigation, Workspace};
use crate::resolve::{ACTIVE, resolve_id, workspace_ids};
use crate::sidebar_drop::{DropIndex, SessionDropTarget, SessionSource, resolve_relative, resolve_sessions};

/// Answers a `session.*` command; those not implemented yet are refused by name.
pub(crate) fn dispatch(core: &mut Core, cmd: SessionCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let target = request.target.as_deref();
    let window = args.window.as_deref();
    match cmd {
        SessionCmd::New => new(core, args),
        SessionCmd::Duplicate => duplicate(core, target, window),
        SessionCmd::Close => close(core, target, args),
        SessionCmd::Select => {
            let (store, id) = session_store(core, target, window)?;
            store.select_session(Some(id));
            Ok(with_id(id))
        }
        SessionCmd::Go => go(core, args),
        SessionCmd::Rename => {
            let name = args
                .name
                .as_deref()
                .ok_or_else(|| ControlError::rejected("session.rename requires a name"))?;
            let (store, id) = session_store(core, target, window)?;
            store.rename_session(id, name);
            Ok(with_id(id))
        }
        SessionCmd::Reveal => reveal(core, target, window),
        SessionCmd::Move => {
            let form = MoveForm::parse(args)?;
            match &args.targets {
                Some(targets) => move_batch(core, targets, window, &form),
                None => move_one(core, target, window, &form),
            }
        }
        SessionCmd::Type
        | SessionCmd::Text
        | SessionCmd::Copy
        | SessionCmd::Paste
        | SessionCmd::SelectAll
        | SessionCmd::Search => super::surface::dispatch_session(core, cmd, request),
        other => Err(ControlError::NotSupported(Command::Session(other))),
    }
}

// MARK: - new and duplicate

/// The argument conflicts `session.new` refuses before resolving anything.
fn validate_new(args: &Args) -> Result<(), ControlError> {
    let anchored = args.after.is_some() || args.before.is_some();
    let message = if args.after.is_some() && args.before.is_some() {
        "use either --after or --before, not both"
    } else if anchored && (args.workspace.is_some() || args.workspace_name.is_some()) {
        // the anchor names its own workspace
        "session.new takes --after/--before or a workspace, not both"
    } else if args.workspace.is_some() && args.workspace_name.is_some() {
        "use either --workspace or --workspace-name, not both"
    } else if args.create_workspace == Some(true) && args.workspace_name.is_none() {
        "--create-workspace requires --workspace-name"
    } else if args.wait == Some(true) && args.command.is_none() {
        // --wait holds the pane after the command exits, so it means nothing without one
        "--wait requires --command"
    } else {
        return Ok(());
    };
    Err(ControlError::rejected(message))
}

/// `session.new`: a session in the `--after`/`--before` anchor's workspace next to it, in the workspace named by
/// `--workspace-name` (created with `--create-workspace`), or in the `--workspace` one (default `active`). The cwd
/// defaults to the home directory and is otherwise taken as given.
fn new(core: &mut Core, args: &Args) -> Outcome {
    validate_new(args)?;
    let no_select = args.no_select == Some(true);
    let spec = NewSession {
        command: args.command.clone(),
        name: args.name.clone(),
        wait: args.wait.unwrap_or(false),
        select: !no_select,
        ..NewSession::at_cwd(args.cwd.clone().unwrap_or_else(|| core.library().home().to_owned()))
    };
    in_placement_store(core, args.window.as_deref(), |store| {
        if let Some(anchor) = args.after.as_deref().or(args.before.as_deref()) {
            let location = anchor_location(store, anchor)?;
            let at = if args.before.is_some() {
                location.index
            } else {
                location.index + 1
            };
            return add_session(store, location.workspace, NewSession { at: Some(at), ..spec });
        }
        if let Some(name) = args.workspace_name.as_deref() {
            let workspace = named_workspace(store, name, args.create_workspace == Some(true), no_select)?;
            return add_session(store, workspace, spec);
        }
        let workspace = workspace_in(store, args.workspace.as_deref().unwrap_or(ACTIVE))?;
        add_session(store, workspace, spec)
    })
}

/// The workspace `--workspace-name` names, created when `create` is set (revealed unless the session is created in
/// the background, so `--no-select` cannot widen the focus filter).
fn named_workspace(store: &mut AppStore, name: &str, create: bool, no_select: bool) -> Result<Uuid, ControlError> {
    // a blank name can neither be found nor created
    if name.trim().is_empty() {
        return Err(ControlError::rejected("workspace name must not be blank"));
    }
    let workspace = if create {
        store.ensure_workspace(name, !no_select)
    } else {
        store.workspace_named(name).map(Workspace::id)
    };
    workspace.ok_or_else(|| {
        ControlError::rejected(format!(
            "no workspace named \"{name}\" (pass --create-workspace to add it)"
        ))
    })
}

fn add_session(store: &mut AppStore, workspace: Uuid, spec: NewSession) -> Outcome {
    store
        .add_session(workspace, spec)
        .map(with_id)
        .ok_or_else(|| ControlError::rejected("could not create session"))
}

/// `session.duplicate`: a fresh shell right after the source, in its focused pane's directory.
fn duplicate(core: &mut Core, target: Option<&str>, window: Option<&str>) -> Outcome {
    let home = core.library().home().to_owned();
    let (store, id) = session_store(core, target, window)?;
    duplicate_in(store, id, &home)
}

fn duplicate_in(store: &mut AppStore, id: Uuid, home: &str) -> Outcome {
    store
        .duplicate_session(id, home)
        .map(with_id)
        .ok_or_else(|| ControlError::rejected("could not duplicate session"))
}

// MARK: - close, go, reveal

/// `session.close`: the target alone closes for good; a `targets` batch of two or more is one undoable soft close
/// while close-grace undo is on (the default), else each closes for good. A batch answers how many it closed; every
/// target must resolve before anything closes.
fn close(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let window = args.window.as_deref();
    let Some(targets) = &args.targets else {
        let (store, id) = session_store(core, target, window)?;
        store.close_session(id);
        return Ok(with_id(id));
    };
    if targets.is_empty() {
        return Err(ControlError::rejected("session.close requires at least one --target"));
    }
    let grace_undo = core.settings().close_grace_undo_enabled.unwrap_or(true);
    let (store, ids) = session_batch_store(core, targets, window)?;
    if let [id] = ids.as_slice() {
        store.close_session(*id);
        return Ok(with_id(*id));
    }
    let affected = if grace_undo {
        if store.soft_close_sessions(&ids) { ids.len() } else { 0 }
    } else {
        ids.iter().filter(|id| store.close_session(**id).is_some()).count()
    };
    Ok(with_affected(affected))
}

/// `session.go`: a relative step of the selection in the `--window` (else frontmost) window.
fn go(core: &mut Core, args: &Args) -> Outcome {
    let direction = args
        .to
        .as_deref()
        .and_then(SessionNavigation::from_wire)
        .ok_or_else(|| {
            ControlError::rejected("session.go requires --to next|prev|first|last|next-attention|prev-attention")
        })?;
    in_placement_store(core, args.window.as_deref(), |store| {
        store.navigate_session(direction);
        store
            .selected_session_id()
            .map(with_id)
            .ok_or_else(|| ControlError::rejected("no session to navigate"))
    })
}

/// `session.reveal`: shows the session's focused-pane directory in the file manager (upstream's
/// `revealSessionInFinder`); refused unless that directory exists.
fn reveal(core: &mut Core, target: Option<&str>, window: Option<&str>) -> Outcome {
    let home = core.library().home().to_owned();
    let (store, id) = session_store(core, target, window)?;
    let directory = store
        .session(id)
        .and_then(|session| existing_directory(session.focused_cwd(), &home))
        .ok_or_else(|| ControlError::rejected("session cwd is not an existing directory"))?;
    core.reveal_directory(&directory);
    Ok(with_id(id))
}

/// `path` with a leading `~` expanded to `home`, when it names an existing directory (upstream's
/// `DirectoryPanelDefaults.existingDirectoryURL`).
fn existing_directory(path: &str, home: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let expanded = match path.strip_prefix('~') {
        Some("") => PathBuf::from(home),
        Some(rest) if rest.starts_with('/') => Path::new(home).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    };
    expanded.is_dir().then_some(expanded)
}

// MARK: - move

/// The one form a `session.move` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MoveForm {
    /// `--to up|down|top|bottom` within the workspace.
    Reorder(ReorderDirection),
    /// To another workspace (id, prefix or `active`), appended.
    Workspace(String),
    /// Next to an anchor session, which names its own workspace.
    Place { anchor: String, after: bool },
}

impl MoveForm {
    /// The form `args` ask for, with upstream's checks in its order.
    fn parse(args: &Args) -> Result<Self, ControlError> {
        if args.after.is_some() && args.before.is_some() {
            return Err(ControlError::rejected("use either --after or --before, not both"));
        }
        if let Some(anchor) = args.after.as_ref().or(args.before.as_ref()) {
            if args.to.is_some() {
                return Err(ControlError::rejected(
                    "session.move takes --after/--before or --to, not both",
                ));
            }
            if args.workspace.is_some() {
                return Err(ControlError::rejected(
                    "session.move takes --after/--before or a workspace, not both",
                ));
            }
            return Ok(Self::Place {
                anchor: anchor.clone(),
                after: args.after.is_some(),
            });
        }
        if args.to.is_some() && args.workspace.is_some() {
            return Err(ControlError::rejected(
                "session.move takes either --to or a workspace, not both",
            ));
        }
        if let Some(to) = args.to.as_deref() {
            let direction = ReorderDirection::from_wire(to)
                .ok_or_else(|| ControlError::rejected("session.move --to must be up|down|top|bottom"))?;
            if args.targets.is_some() {
                return Err(ControlError::rejected(
                    "session.move --target can be repeated only with a workspace or --after/--before",
                ));
            }
            return Ok(Self::Reorder(direction));
        }
        args.workspace
            .clone()
            .map(Self::Workspace)
            .ok_or_else(|| ControlError::rejected("session.move requires --to or a workspace"))
    }
}

/// A batch move: a single target moves as the target alone does, two or more as one block.
fn move_batch(core: &mut Core, targets: &[String], window: Option<&str>, form: &MoveForm) -> Outcome {
    match targets {
        [] => Err(ControlError::rejected("session.move requires at least one --target")),
        [only] => move_one(core, Some(only), window, form),
        _ => {
            let (store, ids) = session_batch_store(core, targets, window)?;
            match form {
                MoveForm::Workspace(workspace) => {
                    let workspace = workspace_in(store, workspace)?;
                    Ok(with_affected(store.move_sessions(&ids, workspace, None)))
                }
                MoveForm::Place { anchor, after } => place_block(store, &ids, anchor, *after),
                MoveForm::Reorder(_) => Err(ControlError::rejected(
                    "session.move --target can be repeated only with a workspace or --after/--before",
                )),
            }
        }
    }
}

/// Moves one session; the workspace or anchor resolves in the session's own window.
fn move_one(core: &mut Core, target: Option<&str>, window: Option<&str>, form: &MoveForm) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    match form {
        MoveForm::Reorder(direction) => store.reorder_session(id, *direction),
        MoveForm::Workspace(workspace) => {
            let workspace = workspace_in(store, workspace)?;
            store.move_session(id, workspace, None);
        }
        MoveForm::Place { anchor, after } => {
            let source = location_of(store, id)?;
            let anchor = anchor_location(store, anchor)?;
            let source = SessionSource {
                workspace: source.workspace,
                index: source.index,
            };
            // `None`: the session is already there
            if let Some(resolution) = resolve_relative(source, anchor, *after) {
                store.move_session(id, resolution.workspace, Some(resolution.destination));
            }
        }
    }
    Ok(with_id(id))
}

/// Places a block of sessions next to an anchor with the sidebar's batch drop math, as one move.
fn place_block(store: &mut AppStore, ids: &[Uuid], anchor: &str, after: bool) -> Outcome {
    let sources = ids
        .iter()
        .map(|id| {
            location_of(store, *id).map(|location| SessionSource {
                workspace: location.workspace,
                index: location.index,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let anchor = anchor_location(store, anchor)?;
    let target = SessionDropTarget::SessionRow {
        workspace: anchor.workspace,
        session_index: anchor.index,
        session_count: anchor.count,
    };
    let child_index = if after {
        DropIndex::OnItem
    } else {
        DropIndex::Before(anchor.index)
    };
    let affected = resolve_sessions(&sources, target, child_index).map_or(0, |resolution| {
        store.move_sessions(ids, resolution.workspace, Some(resolution.destination))
    });
    Ok(with_affected(affected))
}

// MARK: - resolution in one store

/// A workspace id, prefix or `active` (the current workspace) resolved inside `store`.
fn workspace_in(store: &AppStore, target: &str) -> Result<Uuid, ControlError> {
    resolve_id(
        Noun::Workspace,
        target,
        &workspace_ids(store),
        store.current_workspace_id(),
    )
}

/// An `--after`/`--before` anchor resolved across the store's workspaces, so it names its own destination.
fn anchor_location(store: &AppStore, anchor: &str) -> Result<SessionLocation, ControlError> {
    let id = session_in(store, anchor)?;
    location_of(store, id)
}

/// Where a session of `store` sits; the miss is defense in depth, as the id came from the store itself.
fn location_of(store: &AppStore, id: Uuid) -> Result<SessionLocation, ControlError> {
    store
        .session_location(id)
        .ok_or_else(|| ControlError::rejected("no such session"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failures a resolved target cannot reach through dispatch: the store's own guards.
    #[test]
    fn a_vanished_session_or_workspace_is_refused_with_upstreams_message() {
        let mut store = AppStore::new();
        let gone = Uuid::from_u128(1);
        assert_eq!(
            add_session(&mut store, gone, NewSession::at_cwd("/")),
            Err(ControlError::rejected("could not create session"))
        );
        assert_eq!(
            duplicate_in(&mut store, gone, "/home/u"),
            Err(ControlError::rejected("could not duplicate session"))
        );
        assert_eq!(
            location_of(&store, gone),
            Err(ControlError::rejected("no such session"))
        );
        assert_eq!(
            place_block(&mut store, &[gone], "active", true),
            Err(ControlError::rejected("no such session"))
        );
    }

    fn args(edit: impl FnOnce(&mut Args)) -> Args {
        let mut args = Args::default();
        edit(&mut args);
        args
    }

    #[test]
    fn move_forms_parse_in_upstreams_order() {
        let some = |value: &str| Some(value.to_owned());
        assert_eq!(
            MoveForm::parse(&args(|a| a.to = some("top"))),
            Ok(MoveForm::Reorder(ReorderDirection::Top))
        );
        assert_eq!(
            MoveForm::parse(&args(|a| a.workspace = some("dest"))),
            Ok(MoveForm::Workspace("dest".to_owned()))
        );
        assert_eq!(
            MoveForm::parse(&args(|a| a.before = some("anchor"))),
            Ok(MoveForm::Place {
                anchor: "anchor".to_owned(),
                after: false
            })
        );
        // the anchor checks come before the --to and workspace ones
        let error = MoveForm::parse(&args(|a| {
            a.after = some("a");
            a.before = some("b");
            a.to = some("sideways");
        }));
        assert_eq!(
            error,
            Err(ControlError::rejected("use either --after or --before, not both"))
        );
        // an invalid direction is reported before a batch is
        let error = MoveForm::parse(&args(|a| {
            a.to = some("sideways");
            a.targets = Some(vec!["a".to_owned(), "b".to_owned()]);
        }));
        assert_eq!(
            error,
            Err(ControlError::rejected("session.move --to must be up|down|top|bottom"))
        );
    }

    #[test]
    fn only_an_existing_directory_is_revealed_and_a_tilde_is_home() {
        let home = std::env::temp_dir();
        let home_text = home.to_string_lossy().into_owned();
        assert_eq!(existing_directory("~", &home_text), Some(home.clone()));
        assert_eq!(existing_directory(&home_text, "/nowhere"), Some(home.clone()));
        assert_eq!(existing_directory("", &home_text), None);
        assert_eq!(existing_directory("~/agterm-no-such-dir", &home_text), None);
        assert_eq!(existing_directory("~nobody-here", &home_text), None);
    }
}
