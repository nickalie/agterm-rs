//! The command groups' handlers, one module per group: argument validation as upstream's `ControlDispatcher.swift`
//! does it, then the effect as upstream's app-side `agterm/Control/ControlServer+*.swift` performs it on the model.
//! Effects that need the frontend (focusing a terminal, revealing a directory) go through [`crate::Core`]'s hooks.

pub(crate) mod session;
pub(crate) mod window;
pub(crate) mod workspace;

#[cfg(test)]
mod tests;

use agterm_proto::{Args, ControlResult, Request};
use uuid::Uuid;

use crate::Core;
use crate::error::{ControlError, Noun};
use crate::model::AppStore;
use crate::pane_env::uuid_string;
use crate::resolve::{ACTIVE, Resolver, resolve_id, session_ids};

/// What a handler answers: a result object, or the wire error.
pub(crate) type Outcome = Result<ControlResult, ControlError>;

/// The request's args, absent ones read as all unset.
fn args_of(request: &Request) -> &Args {
    static NONE: std::sync::LazyLock<Args> = std::sync::LazyLock::new(Args::default);
    request.args.as_ref().unwrap_or(&NONE)
}

/// `{"id": id}`.
fn with_id(id: Uuid) -> ControlResult {
    ControlResult::id(uuid_string(id))
}

/// `{"affected": count}`.
fn with_affected(count: usize) -> ControlResult {
    ControlResult::affected(i64::try_from(count).unwrap_or(i64::MAX))
}

/// Runs `body` on the store a placement command acts on: the `--window` one, else the frontmost. With every window
/// closed that is an empty throwaway store, as upstream's `ControlTargetResolver.emptyStore`, so a command answers as
/// it would against an empty window and changes nothing.
fn in_placement_store<T>(
    core: &mut Core,
    window: Option<&str>,
    body: impl FnOnce(&mut AppStore) -> Result<T, ControlError>,
) -> Result<T, ControlError> {
    let scoped = Resolver::new(core.library()).placement_window(window)?;
    let mut empty = AppStore::new();
    let store = match scoped.and_then(|id| core.library_mut().store_mut(id)) {
        Some(store) => store,
        None => &mut empty,
    };
    body(store)
}

/// The session target (default `active`) and the store owning it.
fn session_store<'a>(
    core: &'a mut Core,
    target: Option<&str>,
    window: Option<&str>,
) -> Result<(&'a mut AppStore, Uuid), ControlError> {
    let resolved = Resolver::new(core.library()).session(target, window)?;
    let store = core
        .library_mut()
        .store_mut(resolved.window)
        .ok_or_else(|| not_found(Noun::Session, target.unwrap_or(ACTIVE)))?;
    Ok((store, resolved.session))
}

/// The workspace target (default `active`, the current workspace) and the store owning it.
fn workspace_store<'a>(
    core: &'a mut Core,
    target: Option<&str>,
    window: Option<&str>,
) -> Result<(&'a mut AppStore, Uuid), ControlError> {
    let resolved = Resolver::new(core.library()).workspace(target, window)?;
    let store = core
        .library_mut()
        .store_mut(resolved.window)
        .ok_or_else(|| not_found(Noun::Workspace, target.unwrap_or(ACTIVE)))?;
    Ok((store, resolved.workspace))
}

/// A batch of session targets (repeated `--target`) resolved in the first one's window, and that window's store.
fn session_batch_store<'a>(
    core: &'a mut Core,
    targets: &[String],
    window: Option<&str>,
) -> Result<(&'a mut AppStore, Vec<Uuid>), ControlError> {
    let (owner, ids) = Resolver::new(core.library()).session_batch(targets, window)?;
    let first = targets.first().map_or(ACTIVE, String::as_str);
    let store = core
        .library_mut()
        .store_mut(owner)
        .ok_or_else(|| not_found(Noun::Session, first))?;
    Ok((store, ids))
}

/// A session id or prefix resolved inside `store` (`active` is its selection).
fn session_in(store: &AppStore, target: &str) -> Result<Uuid, ControlError> {
    resolve_id(Noun::Session, target, &session_ids(store), store.selected_session_id())
}

fn not_found(noun: Noun, target: &str) -> ControlError {
    ControlError::NotFound {
        noun,
        target: target.to_owned(),
    }
}

/// An on/off/toggle mode argument, `toggle` when absent (upstream's `ControlToggleMode`). Each command keeps its own
/// refusal message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToggleMode {
    On,
    Off,
    Toggle,
}

impl ToggleMode {
    /// Parses `on`, `off` or `toggle`; an absent mode is `toggle`.
    pub(crate) fn parse(mode: Option<&str>) -> Option<Self> {
        match mode.unwrap_or("toggle") {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }

    /// The value the mode asks for, given the `current` one.
    pub(crate) fn desired_value(self, current: bool) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Toggle => !current,
        }
    }
}
