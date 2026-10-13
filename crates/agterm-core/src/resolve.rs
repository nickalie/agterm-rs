//! Target resolution: turning a control target into a window, workspace or session id, as upstream's
//! `ControlResolve.swift` (the string matcher) and `agterm/Control/ControlTargetResolver.swift` (store scoping)
//! do (`control-api.md` > Transport and addressing).
//!
//! A target is `active`, a full id in any case, or a unique prefix of one. Without `--window`, `active` names the
//! frontmost window's selection while an id or prefix matches across every open window; with it, everything resolves
//! inside that open window. When every window is closed the frontmost default is an empty window (upstream's
//! throwaway store), against which nothing resolves.

use uuid::Uuid;

use crate::error::{ControlError, Noun};
use crate::model::{AppStore, WindowLibrary};
use crate::pane_env::uuid_string;

/// The target every command defaults to.
pub const ACTIVE: &str = "active";

/// What a target string matched among a candidate id set (upstream's `TargetResolution`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetResolution {
    Resolved(Uuid),
    NotFound,
    /// Two or more ids share the prefix, in candidate order.
    Ambiguous(Vec<Uuid>),
}

/// Resolves `target` among `candidates`, `active` being the current one: an empty target matches nothing (an empty
/// prefix would match everything), `active` is the current id, an exact id wins in any case, else a case-insensitive
/// prefix must match exactly one id.
#[must_use]
pub fn resolve(target: &str, candidates: &[Uuid], active: Option<Uuid>) -> TargetResolution {
    if target.is_empty() {
        return TargetResolution::NotFound;
    }
    if target == ACTIVE {
        return active.map_or(TargetResolution::NotFound, TargetResolution::Resolved);
    }
    let needle = target.to_lowercase();
    let lowered: Vec<(Uuid, String)> = candidates
        .iter()
        .map(|id| (*id, uuid_string(*id).to_lowercase()))
        .collect();
    if let Some((exact, _)) = lowered.iter().find(|(_, text)| *text == needle) {
        return TargetResolution::Resolved(*exact);
    }
    let hits: Vec<Uuid> = lowered
        .iter()
        .filter(|(_, text)| text.starts_with(&needle))
        .map(|(id, _)| *id)
        .collect();
    match hits.as_slice() {
        [] => TargetResolution::NotFound,
        [only] => TargetResolution::Resolved(*only),
        _ => TargetResolution::Ambiguous(hits),
    }
}

/// The wire error of a resolution that did not settle on an owned id: the ambiguous-prefix message for an ambiguous
/// one, the not-found message otherwise, a resolved id whose owner is gone included.
#[must_use]
pub fn resolution_error(noun: Noun, target: &str, resolution: TargetResolution) -> ControlError {
    match resolution {
        TargetResolution::Ambiguous(hits) => ControlError::Ambiguous {
            noun,
            target: target.to_owned(),
            hits: hits.into_iter().map(uuid_string).collect(),
        },
        TargetResolution::Resolved(_) | TargetResolution::NotFound => ControlError::NotFound {
            noun,
            target: target.to_owned(),
        },
    }
}

/// [`resolve`], with the wire error for anything but a match.
///
/// # Errors
///
/// [`ControlError::NotFound`] or [`ControlError::Ambiguous`].
pub fn resolve_id(noun: Noun, target: &str, candidates: &[Uuid], active: Option<Uuid>) -> Result<Uuid, ControlError> {
    match resolve(target, candidates, active) {
        TargetResolution::Resolved(id) => Ok(id),
        other => Err(resolution_error(noun, target, other)),
    }
}

/// `value` without surrounding whitespace, `None` when absent or blank (upstream's `trimmed`).
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Every session id of a store, workspace by workspace.
#[must_use]
pub fn session_ids(store: &AppStore) -> Vec<Uuid> {
    store.sessions().map(crate::model::Session::id).collect()
}

/// Every workspace id of a store, in sidebar order.
#[must_use]
pub fn workspace_ids(store: &AppStore) -> Vec<Uuid> {
    store.workspaces().iter().map(crate::model::Workspace::id).collect()
}

/// A resolved session and the open window that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTarget {
    pub window: Uuid,
    pub session: Uuid,
}

/// A resolved workspace and the open window that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceTarget {
    pub window: Uuid,
    pub workspace: Uuid,
}

/// The target resolver over the window library.
#[derive(Debug, Clone, Copy)]
pub struct Resolver<'a> {
    library: &'a WindowLibrary,
}

impl<'a> Resolver<'a> {
    #[must_use]
    pub fn new(library: &'a WindowLibrary) -> Self {
        Self { library }
    }

    /// A window target (default `active`, the frontmost open window) to its id; the window need not be open.
    ///
    /// # Errors
    ///
    /// The target matches no window, or several.
    pub fn window_id(&self, target: Option<&str>) -> Result<Uuid, ControlError> {
        let target = target.unwrap_or(ACTIVE);
        let ids: Vec<Uuid> = self.library.windows().iter().map(|info| info.id).collect();
        resolve_id(Noun::Window, target, &ids, self.library.active_window_id())
    }

    /// The open window a placement or `active` command acts on: the `--window` one, else the frontmost. `None` when
    /// no `--window` was given and every window is closed: the empty-window default.
    ///
    /// # Errors
    ///
    /// The `--window` target does not resolve, or names a closed window.
    pub fn placement_window(&self, window: Option<&str>) -> Result<Option<Uuid>, ControlError> {
        let Some(window) = trimmed(window) else {
            return Ok(self.library.active_window_id());
        };
        let id = self.window_id(Some(window))?;
        if self.library.is_open(id) {
            Ok(Some(id))
        } else {
            Err(ControlError::WindowNotOpen)
        }
    }

    /// [`Resolver::placement_window`] for a window-scoped command that must fail rather than act on nothing when no
    /// window is open.
    ///
    /// # Errors
    ///
    /// As [`Resolver::placement_window`], plus [`ControlError::NoOpenWindow`] without `--window` once every window
    /// is closed.
    pub fn open_placement_window(&self, window: Option<&str>) -> Result<Uuid, ControlError> {
        self.placement_window(window)?.ok_or(ControlError::NoOpenWindow)
    }

    /// A session target (default `active`): inside the `--window` window when one is named, else `active` in the
    /// frontmost window and an id or prefix across every open window.
    ///
    /// # Errors
    ///
    /// The window or the session does not resolve.
    pub fn session(&self, target: Option<&str>, window: Option<&str>) -> Result<SessionTarget, ControlError> {
        let scoped = self.placement_window(window)?;
        let target = target.unwrap_or(ACTIVE);
        if trimmed(window).is_some() || target == ACTIVE {
            let store = scoped.and_then(|id| self.library.store(id));
            let candidates = store.map(session_ids).unwrap_or_default();
            let active = store.and_then(AppStore::selected_session_id);
            let session = resolve_id(Noun::Session, target, &candidates, active)?;
            // a match comes from the scoped store's candidates, so the scope is a window
            let window = scoped.ok_or_else(|| resolution_error(Noun::Session, target, TargetResolution::NotFound))?;
            return Ok(SessionTarget { window, session });
        }
        let (window, session) = self.across_windows(Noun::Session, target, session_ids)?;
        Ok(SessionTarget { window, session })
    }

    /// A workspace target (default `active`, the window's current workspace), scoped as [`Resolver::session`].
    ///
    /// # Errors
    ///
    /// The window or the workspace does not resolve.
    pub fn workspace(&self, target: Option<&str>, window: Option<&str>) -> Result<WorkspaceTarget, ControlError> {
        let scoped = self.placement_window(window)?;
        let target = target.unwrap_or(ACTIVE);
        if trimmed(window).is_some() || target == ACTIVE {
            let store = scoped.and_then(|id| self.library.store(id));
            let candidates = store.map(workspace_ids).unwrap_or_default();
            let active = store.and_then(AppStore::current_workspace_id);
            let workspace = resolve_id(Noun::Workspace, target, &candidates, active)?;
            let window = scoped.ok_or_else(|| resolution_error(Noun::Workspace, target, TargetResolution::NotFound))?;
            return Ok(WorkspaceTarget { window, workspace });
        }
        let (window, workspace) = self.across_windows(Noun::Workspace, target, workspace_ids)?;
        Ok(WorkspaceTarget { window, workspace })
    }

    /// A batch of session targets (repeated `--target`): the first resolves as [`Resolver::session`], the rest
    /// inside the first one's window; duplicates drop, order is kept, and any miss fails the whole batch.
    ///
    /// # Errors
    ///
    /// [`ControlError::BatchNeedsTarget`] for an empty batch, else the first target that does not resolve.
    pub fn session_batch(&self, targets: &[String], window: Option<&str>) -> Result<(Uuid, Vec<Uuid>), ControlError> {
        let (first, rest) = targets.split_first().ok_or(ControlError::BatchNeedsTarget)?;
        let first = self.session(Some(first), window)?;
        let store = self.library.store(first.window);
        let candidates = store.map(session_ids).unwrap_or_default();
        let active = store.and_then(AppStore::selected_session_id);
        let mut ids = vec![first.session];
        for target in rest {
            let id = resolve_id(Noun::Session, target, &candidates, active)?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok((first.window, ids))
    }

    /// Matches an id or prefix against the ids of every open window together, then maps the match to its owner.
    fn across_windows(
        self,
        noun: Noun,
        target: &str,
        candidates: fn(&AppStore) -> Vec<Uuid>,
    ) -> Result<(Uuid, Uuid), ControlError> {
        let stores: Vec<(Uuid, Vec<Uuid>)> = self
            .library
            .open_ids()
            .into_iter()
            .filter_map(|window| self.library.store(window).map(|store| (window, candidates(store))))
            .collect();
        let all: Vec<Uuid> = stores.iter().flat_map(|(_, ids)| ids.iter().copied()).collect();
        let resolution = resolve(target, &all, None);
        if let TargetResolution::Resolved(id) = resolution
            && let Some((window, _)) = stores.iter().find(|(_, ids)| ids.contains(&id))
        {
            return Ok((*window, id));
        }
        Err(resolution_error(noun, target, resolution))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NewSession;

    const A: Uuid = Uuid::from_u128(0xA1B2_C3D4_0000_0000_0000_0000_0000_0001);
    const B: Uuid = Uuid::from_u128(0xA9B8_C7D6_0000_0000_0000_0000_0000_0002);
    const C: Uuid = Uuid::from_u128(0x0123_4567_0000_0000_0000_0000_0000_0003);

    #[test]
    fn matches_active_exact_ids_and_unique_prefixes() {
        let ids = [A, B, C];
        assert_eq!(resolve("active", &ids, Some(B)), TargetResolution::Resolved(B));
        assert_eq!(resolve("active", &ids, None), TargetResolution::NotFound);
        assert_eq!(resolve(&uuid_string(A), &ids, None), TargetResolution::Resolved(A));
        assert_eq!(
            resolve(&uuid_string(A).to_lowercase(), &ids, None),
            TargetResolution::Resolved(A)
        );
        assert_eq!(resolve("a1b2", &ids, None), TargetResolution::Resolved(A));
        assert_eq!(resolve("A9", &ids, None), TargetResolution::Resolved(B));
        assert_eq!(resolve("a", &ids, None), TargetResolution::Ambiguous(vec![A, B]));
        assert_eq!(resolve("zzz", &ids, None), TargetResolution::NotFound);
    }

    #[test]
    fn an_empty_target_matches_nothing_and_active_is_literal() {
        assert_eq!(resolve("", &[A], Some(A)), TargetResolution::NotFound);
        assert_eq!(resolve("ACTIVE", &[A], Some(A)), TargetResolution::NotFound);
        assert_eq!(resolve(" a1b2", &[A], None), TargetResolution::NotFound);
    }

    #[test]
    fn the_errors_are_the_pinned_wire_strings() {
        let ambiguous = resolve_id(Noun::Workspace, "a", &[A, B], None).unwrap_err();
        assert_eq!(
            ambiguous.to_string(),
            "ambiguous workspace prefix 'a' → A1B2C3D4, A9B8C7D6"
        );
        let missing = resolve_id(Noun::Session, "zzz", &[A], None).unwrap_err();
        assert_eq!(missing.to_string(), "no such session: zzz");
        let empty = resolve_id(Noun::Session, "", &[A], None).unwrap_err();
        assert_eq!(empty.to_string(), "no such session: ");
        // an id whose owner is gone reads as a miss
        let orphan = resolution_error(Noun::Window, "w", TargetResolution::Resolved(A));
        assert_eq!(orphan.to_string(), "no such window: w");
    }

    /// Two open windows, the second frontmost, each with one workspace and one session.
    fn two_windows() -> (WindowLibrary, [Uuid; 2]) {
        let mut library = WindowLibrary::new("/home/u");
        let first = library.new_window(None);
        let second = library.new_window(None);
        (library, [first, second])
    }

    fn only_session(library: &WindowLibrary, window: Uuid) -> Uuid {
        session_ids(library.store(window).unwrap())[0]
    }

    #[test]
    fn windows_resolve_by_id_prefix_and_frontmost_open_or_not() {
        let (mut library, [first, second]) = two_windows();
        let resolver = Resolver::new(&library);
        assert_eq!(resolver.window_id(None), Ok(second));
        assert_eq!(resolver.window_id(Some("active")), Ok(second));
        let prefix = uuid_string(first)[..8].to_lowercase();
        assert_eq!(resolver.window_id(Some(&prefix)), Ok(first));
        assert_eq!(
            resolver.window_id(Some("zzz")).unwrap_err().to_string(),
            "no such window: zzz"
        );
        library.close_window(first);
        assert_eq!(Resolver::new(&library).window_id(Some(&uuid_string(first))), Ok(first));
    }

    #[test]
    fn placement_takes_the_named_open_window_else_the_frontmost() {
        let (mut library, [first, second]) = two_windows();
        let resolver = Resolver::new(&library);
        assert_eq!(resolver.placement_window(None), Ok(Some(second)));
        assert_eq!(resolver.placement_window(Some("  ")), Ok(Some(second)));
        assert_eq!(resolver.placement_window(Some(&uuid_string(first))), Ok(Some(first)));
        library.close_window(first);
        let resolver = Resolver::new(&library);
        assert_eq!(
            resolver
                .placement_window(Some(&uuid_string(first)))
                .unwrap_err()
                .to_string(),
            "window not open — window.select it first"
        );
        library.close_window(second);
        let resolver = Resolver::new(&library);
        assert_eq!(resolver.placement_window(None), Ok(None));
        assert_eq!(
            resolver.open_placement_window(None).unwrap_err().to_string(),
            "no open window"
        );
    }

    #[test]
    fn active_names_the_frontmost_selection_while_ids_cross_windows() {
        let (library, [first, second]) = two_windows();
        let (in_first, in_second) = (only_session(&library, first), only_session(&library, second));
        let resolver = Resolver::new(&library);
        let target = |window, session| SessionTarget { window, session };
        assert_eq!(resolver.session(None, None), Ok(target(second, in_second)));
        assert_eq!(resolver.session(Some("active"), None), Ok(target(second, in_second)));
        assert_eq!(
            resolver.session(Some(&uuid_string(in_first)), None),
            Ok(target(first, in_first))
        );
        let prefix = uuid_string(in_first)[..8].to_lowercase();
        assert_eq!(resolver.session(Some(&prefix), None), Ok(target(first, in_first)));
        // a named window scopes the id and `active` to it
        let named = uuid_string(first);
        assert_eq!(resolver.session(None, Some(&named)), Ok(target(first, in_first)));
        assert_eq!(
            resolver
                .session(Some(&uuid_string(in_second)), Some(&named))
                .unwrap_err()
                .to_string(),
            format!("no such session: {}", uuid_string(in_second))
        );
        assert_eq!(
            resolver.session(Some(""), None).unwrap_err().to_string(),
            "no such session: "
        );
    }

    #[test]
    fn workspaces_resolve_to_the_current_one_and_across_windows() {
        let (mut library, [first, second]) = two_windows();
        let added = library.store_mut(second).unwrap().add_workspace("two");
        let current = library.store(second).unwrap().current_workspace_id();
        assert_eq!(current, Some(added), "a new workspace becomes current");
        let resolver = Resolver::new(&library);
        let target = |window, workspace| WorkspaceTarget { window, workspace };
        assert_eq!(resolver.workspace(None, None), Ok(target(second, added)));
        let in_first = workspace_ids(library.store(first).unwrap())[0];
        assert_eq!(
            resolver.workspace(Some(&uuid_string(in_first)), None),
            Ok(target(first, in_first))
        );
        assert_eq!(
            resolver.workspace(Some("zzz"), None).unwrap_err().to_string(),
            "no such workspace: zzz"
        );
    }

    #[test]
    fn a_prefix_shared_across_windows_is_ambiguous_in_window_order() {
        let (mut library, [first, second]) = two_windows();
        // seventeen workspaces always hold two whose ids share a first hex digit
        for _ in 0..8 {
            library.store_mut(first).unwrap().add_workspace("w");
            library.store_mut(second).unwrap().add_workspace("w");
        }
        let all: Vec<String> = [first, second]
            .iter()
            .flat_map(|window| workspace_ids(library.store(*window).unwrap()))
            .map(uuid_string)
            .collect();
        let starts = |digit: &str| -> Vec<String> {
            all.iter()
                .filter(|id| id.to_lowercase().starts_with(digit))
                .cloned()
                .collect()
        };
        let shared = all
            .iter()
            .map(|id| id[..1].to_lowercase())
            .find(|digit| starts(digit).len() > 1)
            .unwrap();
        let error = Resolver::new(&library).workspace(Some(&shared), None).unwrap_err();
        let ControlError::Ambiguous { hits, .. } = &error else {
            panic!("{error}")
        };
        assert_eq!(hits, &starts(&shared));
    }

    #[test]
    fn a_batch_resolves_in_the_first_targets_window_deduplicated_and_atomic() {
        let (mut library, [first, second]) = two_windows();
        let base = only_session(&library, first);
        let workspace = workspace_ids(library.store(first).unwrap())[0];
        let extra = library
            .store_mut(first)
            .unwrap()
            .add_session(workspace, NewSession::at_cwd("/tmp"))
            .unwrap();
        let other = only_session(&library, second);
        let resolver = Resolver::new(&library);
        let targets = |ids: &[Uuid]| ids.iter().map(|id| uuid_string(*id)).collect::<Vec<_>>();
        assert_eq!(
            resolver.session_batch(&targets(&[extra, base, extra]), None),
            Ok((first, vec![extra, base]))
        );
        // `active` in the rest is the selection of the first target's window
        let mut batch = targets(&[base]);
        batch.push("active".to_owned());
        assert_eq!(resolver.session_batch(&batch, None), Ok((first, vec![base, extra])));
        let error = resolver.session_batch(&targets(&[base, other]), None).unwrap_err();
        assert_eq!(error.to_string(), format!("no such session: {}", uuid_string(other)));
        assert_eq!(
            resolver.session_batch(&[], None).unwrap_err().to_string(),
            "session command requires at least one --target"
        );
    }

    #[test]
    fn with_every_window_closed_nothing_resolves() {
        let (mut library, [first, second]) = two_windows();
        library.close_window(first);
        library.close_window(second);
        let resolver = Resolver::new(&library);
        assert_eq!(
            resolver.session(None, None).unwrap_err().to_string(),
            "no such session: active"
        );
        assert_eq!(
            resolver.workspace(None, None).unwrap_err().to_string(),
            "no such workspace: active"
        );
    }
}
