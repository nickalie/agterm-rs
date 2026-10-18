//! `surface.cursor` and `surface.zoom` over the surface vocabulary of [`crate::zoom`]: `surface:<session-id>:<kind>`
//! ids from `tree`, `quick`, or `active` (the default) for the frontmost or `--window` window's active surface.
//! Upstream's `readSurfaceCursor` (`ControlServer+SurfaceIO.swift`) and `setSurfaceZoom`, `resolveOpenWindow`,
//! `resolveSurfaceOwner` (`ControlServer+SessionActions.swift`).
//!
//! `quick` names the detached quick-terminal panel, which no window's zoom can hold: [`super::quick`] answers it.

use agterm_host::Surface;
use agterm_proto::command::SurfaceCmd;
use agterm_proto::{ControlCursor, ControlResult, Request};
use uuid::Uuid;

use super::live::PaneSource;
use super::quick::{self, QUICK};
use super::{Outcome, ToggleMode, args_of, session_store, unknown_pane_id};
use crate::Core;
use crate::error::ControlError;
use crate::model::{AppStore, StatusPane};
use crate::resolve::{Resolver, trimmed};
use crate::zoom::{SurfaceId, ZoomSurface, apply_mode};

const ACTIVE: &str = "active";

/// Answers a `surface.*` command; `None` is a bare `ok`.
pub(crate) fn dispatch(
    core: &mut Core,
    cmd: SurfaceCmd,
    request: &Request,
) -> Result<Option<ControlResult>, ControlError> {
    let args = args_of(request);
    let target = trimmed(request.target.as_deref()).unwrap_or(ACTIVE);
    let window = args.window.as_deref();
    match cmd {
        SurfaceCmd::Cursor => cursor(core, target, window, args.pane_id.as_deref()).map(Some),
        SurfaceCmd::Zoom => {
            let mode = ToggleMode::parse_with(args.mode.as_deref(), "show", "hide").ok_or_else(|| {
                ControlError::rejected(format!(
                    "invalid surface zoom mode: {}",
                    args.mode.as_deref().unwrap_or("toggle")
                ))
            })?;
            zoom(core, target, window, mode)
        }
    }
}

fn not_available(id: &str) -> ControlError {
    ControlError::rejected(format!("surface not available: {id}"))
}

fn invalid_surface(target: &str) -> ControlError {
    ControlError::rejected(format!("invalid surface: {target}"))
}

/// The open window owning a surface id: the `--window` one, which must hold the session, else the open window that
/// does (upstream's `resolveSurfaceOwner`).
fn surface_owner(core: &Core, id: SurfaceId, window: Option<&str>) -> Result<Uuid, ControlError> {
    let no_such = || ControlError::rejected(format!("no such surface: {}", id.control_id()));
    let library = core.library();
    if trimmed(window).is_some() {
        let owner = Resolver::new(library).open_placement_window(window)?;
        return library
            .store(owner)
            .filter(|store| store.session(id.session).is_some())
            .map(|_| owner)
            .ok_or_else(no_such);
    }
    library.window_id_for_session(id.session).ok_or_else(no_such)
}

/// The store of an open window; the miss is defense in depth, as the window came from the library itself.
fn open_store(core: &Core, window: Uuid) -> Result<&AppStore, ControlError> {
    core.library().store(window).ok_or(ControlError::WindowNotOpen)
}

// MARK: - surface.cursor

/// `surface.cursor`: the addressed surface's cursor column. It changes nothing, so it neither selects nor waits: a
/// terminal not up is reported. A `--pane-id` takes a session target and resolves the pane carrying it.
fn cursor(
    core: &mut Core,
    target: &str,
    window: Option<&str>,
    pane_id: Option<&str>,
) -> Result<ControlResult, ControlError> {
    if let Some(pane_id) = pane_id.filter(|pane_id| !pane_id.is_empty()) {
        if target == QUICK || SurfaceId::parse(target).is_some() {
            return Err(ControlError::rejected(
                "surface.cursor: --pane-id takes a session target",
            ));
        }
        let (store, session) = session_store(core, Some(target), window)?;
        let pane = store
            .session(session)
            .and_then(|node| node.pane_role_for_token(pane_id))
            .ok_or_else(|| unknown_pane_id(pane_id))?;
        let id = SurfaceId {
            session,
            surface: zoom_surface(pane),
        };
        let core: &Core = core;
        let store = core
            .library()
            .store_for_session(session)
            .ok_or(ControlError::WindowNotOpen)?;
        return cursor_of(core, store, id);
    }
    if target == QUICK {
        return cursor_answer(quick::cursor_surface(core)?, QUICK.to_owned());
    }
    let (window, id) = if target == ACTIVE {
        let window = Resolver::new(core.library()).open_placement_window(window)?;
        let store = open_store(core, window)?;
        // a live zoom is the active surface: it wins over the store's focused pane
        let id = core
            .zoom_target(window)
            .or_else(|| SurfaceId::active_in(store))
            .ok_or_else(|| ControlError::rejected("no active surface"))?;
        (window, id)
    } else {
        let id = SurfaceId::parse(target).ok_or_else(|| invalid_surface(target))?;
        (surface_owner(core, id, window)?, id)
    };
    cursor_of(core, open_store(core, window)?, id)
}

fn zoom_surface(pane: StatusPane) -> ZoomSurface {
    match pane {
        StatusPane::Left => ZoomSurface::Primary,
        StatusPane::Right => ZoomSurface::Split,
        StatusPane::Scratch => ZoomSurface::Scratch,
    }
}

/// The cursor of a valid surface id's terminal.
fn cursor_of(core: &Core, store: &AppStore, id: SurfaceId) -> Result<ControlResult, ControlError> {
    let control_id = id.control_id();
    let session = store
        .session(id.session)
        .filter(|session| id.surface.is_available(session))
        .ok_or_else(|| not_available(&control_id))?;
    let surface = id
        .surface
        .slot(session)
        .get()
        .ok_or_else(|| not_available(&control_id))?;
    if !surface.is_realized() {
        return cursor_answer(surface, control_id);
    }
    match core.pane_source(surface, false) {
        PaneSource::Surface => cursor_answer(surface, control_id),
        PaneSource::Refused(reason) => Err(ControlError::rejected(reason)),
        // a pane that took part in leadership reads its cursor from its daemon's grid
        PaneSource::Daemon(name) => {
            let failed = || ControlError::rejected("failed to read cursor position");
            let screen = core
                .live
                .host()
                .ok_or_else(failed)?
                .screen(&name, false)
                .map_err(|_| failed())?;
            Ok(ControlResult {
                cursor: Some(ControlCursor {
                    column: i64::from(screen.cursor_column),
                }),
                ..ControlResult::id(control_id)
            })
        }
    }
}

/// The cursor of a terminal addressed as `control_id` (upstream's `cursorResponse`): a terminal not up is reported,
/// not waited for.
fn cursor_answer(surface: &dyn Surface, control_id: String) -> Outcome {
    if !surface.is_realized() {
        return Err(ControlError::rejected("surface not realized"));
    }
    let column = surface
        .cursor_column()
        .ok_or_else(|| ControlError::rejected("failed to read cursor position"))?;
    Ok(ControlResult {
        cursor: Some(ControlCursor {
            column: i64::from(column),
        }),
        ..ControlResult::id(control_id)
    })
}

// MARK: - surface.zoom

/// `surface.zoom`: shows, hides or toggles a window's terminal zoom on the addressed surface. `hide` is idempotent and
/// skips the availability check, since the surface may be gone while the end state holds.
fn zoom(
    core: &mut Core,
    target: &str,
    window: Option<&str>,
    mode: ToggleMode,
) -> Result<Option<ControlResult>, ControlError> {
    if target == ACTIVE {
        return active_zoom(core, window, mode);
    }
    if target == QUICK {
        return quick::set_zoom(core, mode).map(Some);
    }
    let id = SurfaceId::parse(target).ok_or_else(|| invalid_surface(target))?;
    let owner = surface_owner(core, id, window)?;
    let current = core.zoom_target(owner);
    if mode.desired_value(current == Some(id))
        && current != Some(id)
        && let Some(error) = core.modals.pending_modal_error(owner)
    {
        return Err(ControlError::rejected(error));
    }
    if mode != ToggleMode::Off && !id.is_valid_in(open_store(core, owner)?) {
        return Err(not_available(&id.control_id()));
    }
    let zoomed = apply_mode(core.zoom_target(owner), mode, Some(id));
    core.set_zoom_target(owner, zoomed);
    Ok(Some(ControlResult::id(id.control_id())))
}

/// The `active` form: acts on the current zoom when one is up, else on the window's active surface; hiding with
/// nothing zoomed is a bare `ok`.
fn active_zoom(core: &mut Core, window: Option<&str>, mode: ToggleMode) -> Result<Option<ControlResult>, ControlError> {
    let window = Resolver::new(core.library()).open_placement_window(window)?;
    let current = core.zoom_target(window);
    if current.is_none()
        && mode != ToggleMode::Off
        && let Some(error) = core.modals.pending_modal_error(window)
    {
        return Err(ControlError::rejected(error));
    }
    let effective = match current {
        Some(current) => current,
        None if mode == ToggleMode::Off => return Ok(None),
        None => {
            let id = SurfaceId::active_in(open_store(core, window)?)
                .ok_or_else(|| ControlError::rejected("no active surface"))?;
            if !id.is_valid_in(open_store(core, window)?) {
                return Err(not_available(&id.control_id()));
            }
            id
        }
    };
    core.set_zoom_target(window, apply_mode(current, mode, Some(effective)));
    Ok(Some(ControlResult::id(effective.control_id())))
}
