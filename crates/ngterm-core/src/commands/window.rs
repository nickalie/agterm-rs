//! `window.*`: `new`, `list`, `select`, `go`, `close`, `rename`, `delete`, `resize`, `move`, `zoom`, `fullscreen` and
//! `minimize`. Validation and its messages follow `dispatchWindowCommand` in upstream's `ControlDispatcher.swift`; the
//! effects follow `agterm/Control/ControlServer+WindowCommands.swift` over `WindowLibrary` and `WindowRegistry.swift`.
//!
//! The model changes here; what needs the on-screen window goes to the frontend as a [`WindowRequest`], applied
//! after the commit, by which time the frontend has built a window for every store this request opened and dropped
//! the window of every store it closed. So upstream's bounded polls for a window to attach or detach have nothing to
//! wait for: an open store is an on-screen window once the reply is out. Upstream's `window.list` is a cache refreshed
//! off the main actor; here it is computed on dispatch.

use ngterm_proto::command::WindowCmd;
use ngterm_proto::{Args, ControlResult, ControlWindowNode, Request};
use uuid::Uuid;

use super::{Outcome, ToggleMode, args_of, with_id};
use crate::Core;
use crate::error::ControlError;
use crate::model::WorkspaceNavigation;
use crate::model::naming::trimmed_or_none;
use crate::pane_env::uuid_string;
use crate::resolve::Resolver;
use crate::windows::{MIN_WINDOW_SIZE, WindowRequest, WindowSize, clamp_size};

/// The capability of a host whose windowing system lets a client place its window and read its frame back.
const WINDOW_GEOMETRY: &str = "window-geometry";

/// `window.move` on a host without [`WINDOW_GEOMETRY`] (the Linux fork's wording; Wayland gives a client no window
/// position).
const MOVE_UNSUPPORTED: &str =
    "window.move is not supported on this platform (the compositor controls window position)";

/// Answers a `window.*` command. Arguments are checked before the target resolves, as upstream's dispatcher does.
pub(crate) fn dispatch(core: &mut Core, cmd: WindowCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let target = request.target.as_deref();
    match cmd {
        WindowCmd::New => Ok(new(core, args)),
        WindowCmd::List => Ok(ControlResult::windows(window_nodes(core))),
        WindowCmd::Select => select(core, target),
        WindowCmd::Go => go(core, args),
        WindowCmd::Close => close(core, target),
        WindowCmd::Rename => rename(core, target, args),
        WindowCmd::Delete => delete(core, target),
        WindowCmd::Resize => resize(core, target, args),
        WindowCmd::Move => move_window(core, target, args),
        WindowCmd::Zoom => toggle(core, target, WindowRequest::ToggleZoom),
        WindowCmd::Fullscreen => toggle(core, target, WindowRequest::ToggleFullscreen),
        WindowCmd::Minimize => minimize(core, target, args),
    }
}

/// Every window in window order with its open and frontmost flags. An open window adds its auto-follow timeout (absent
/// while the feature is off) and sidebar visibility, and what the frontend reports of its on-screen state; a closed
/// window has none of those. The frame stays absent: a Wayland client cannot read its window's position.
pub(crate) fn window_nodes(core: &Core) -> Vec<ControlWindowNode> {
    let library = core.library();
    let active = library.active_window_id();
    let auto_follow_ms = core
        .settings()
        .effective_auto_follow_attention()
        .timeout_secs()
        .map(|secs| i64::from(secs) * 1000);
    library
        .windows()
        .iter()
        .map(|info| {
            let store = library.store(info.id);
            // a state the frontend has not withdrawn yet still says nothing about a closed window
            let live = store.and(core.window_state(info.id));
            ControlWindowNode {
                id: uuid_string(info.id),
                name: info.name.clone(),
                open: store.is_some(),
                active: Some(info.id) == active,
                auto_follow_ms: store.and(auto_follow_ms),
                sidebar_visible: store.map(|store| store.sidebar().visible),
                geometry: None,
                fullscreen: live.map(|state| state.fullscreen),
                zoomed: live.map(|state| state.zoomed),
                minimized: live.map(|state| state.minimized.unwrap_or(false)),
            }
        })
        .collect()
}

/// The window a target names (default `active`), open or closed.
fn window_id(core: &Core, target: Option<&str>) -> Result<Uuid, ControlError> {
    Resolver::new(core.library()).window_id(target)
}

/// The window a target names, which must be on screen.
fn open_window_id(core: &Core, target: Option<&str>) -> Result<Uuid, ControlError> {
    let id = window_id(core, target)?;
    if core.library().is_open(id) {
        Ok(id)
    } else {
        Err(ControlError::WindowNotOpen)
    }
}

/// `window.new`: a window seeded with a workspace and a session at home, named after `--name` (else "window N"),
/// opened and made frontmost; `--minimized` parks it once created, leaving frontmost on a window still in view.
fn new(core: &mut Core, args: &Args) -> ControlResult {
    let id = core.library_mut().new_window(args.name.as_deref());
    // already frontmost: this only counts the take, so a frontend holds back an earlier new window's late focus
    core.take_frontmost(id);
    if args.minimized.unwrap_or(false) {
        core.request_window(WindowRequest::Minimize {
            window: id,
            minimized: true,
        });
        hand_off_frontmost(core, id);
    }
    with_id(id)
}

/// `window.select`: raises an open window or opens a closed one from its file, and makes it frontmost.
fn select(core: &mut Core, target: Option<&str>) -> Outcome {
    let id = window_id(core, target)?;
    core.open_window(id);
    core.request_window(WindowRequest::Raise(id));
    core.take_frontmost(id);
    Ok(with_id(id))
}

/// `window.go --to next|prev`: raises the next or previous open window, wrapping, and makes it frontmost. Closed
/// windows are no stops.
fn go(core: &mut Core, args: &Args) -> Outcome {
    let direction = args
        .to
        .as_deref()
        .and_then(WorkspaceNavigation::from_wire)
        .ok_or_else(|| ControlError::rejected("window.go requires --to next|prev"))?;
    let id = core
        .library()
        .navigate_window(direction)
        .ok_or_else(|| ControlError::rejected("no other open window to navigate to"))?;
    core.request_window(WindowRequest::Raise(id));
    core.take_frontmost(id);
    Ok(with_id(id))
}

/// `window.close`: closes the window, its snapshot written first; its bundle stays in the library. An already closed
/// window answers ok.
fn close(core: &mut Core, target: Option<&str>) -> Outcome {
    let id = window_id(core, target)?;
    core.close_window(id);
    Ok(with_id(id))
}

/// `window.rename`: the trimmed `--name`, required; it lives in the index.
fn rename(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let name = args
        .name
        .as_deref()
        .and_then(trimmed_or_none)
        .ok_or_else(|| ControlError::rejected("window.rename requires a name"))?;
    let id = window_id(core, target)?;
    core.library_mut().rename_window(id, &name);
    Ok(with_id(id))
}

/// `window.delete`: closes the window if open and removes it with its file; the last window is kept.
fn delete(core: &mut Core, target: Option<&str>) -> Outcome {
    let id = window_id(core, target)?;
    if !core.library().can_remove_window() {
        return Err(ControlError::rejected("cannot delete last window"));
    }
    core.remove_window(id);
    Ok(with_id(id))
}

/// `window.resize`: positive `--width` and `--height`, clamped between the window minimum and the display; echoes the
/// applied size. The frontend takes a maximized window out of that state first (as `setFrame` does to a zoomed
/// `NSWindow`), so the echoed size is the one the window takes. A full-screen window stays in full screen, as upstream
/// leaves it only through `window.fullscreen`: nothing is requested, and the echo is the size it keeps, the display's.
fn resize(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let requested = match (args.width, args.height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => WindowSize { width, height },
        _ => {
            return Err(ControlError::rejected(
                "window.resize requires positive width and height",
            ));
        }
    };
    let id = open_window_id(core, target)?;
    let state = core.window_state(id).unwrap_or_default();
    let size = clamp_size(requested, MIN_WINDOW_SIZE, state.display);
    if state.fullscreen {
        let kept = state.display.unwrap_or(size);
        return Ok(ControlResult::sized(uuid_string(id), kept.width, kept.height));
    }
    core.request_window(WindowRequest::Resize { window: id, size });
    Ok(ControlResult::sized(uuid_string(id), size.width, size.height))
}

/// `window.move`: `--x` and `--y` required. A host without [`WINDOW_GEOMETRY`] refuses it once the target resolves.
fn move_window(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let (Some(x), Some(y)) = (args.x, args.y) else {
        return Err(ControlError::rejected("window.move requires x and y"));
    };
    let id = window_id(core, target)?;
    if !core.has_capability(WINDOW_GEOMETRY) {
        return Err(ControlError::rejected(MOVE_UNSUPPORTED));
    }
    if !core.library().is_open(id) {
        return Err(ControlError::WindowNotOpen);
    }
    core.request_window(WindowRequest::Move {
        window: id,
        x,
        y,
        display: args.display,
    });
    Ok(with_id(id))
}

/// `window.zoom` and `window.fullscreen`: a toggle of an open window.
fn toggle(core: &mut Core, target: Option<&str>, request: fn(Uuid) -> WindowRequest) -> Outcome {
    let id = open_window_id(core, target)?;
    core.request_window(request(id));
    Ok(with_id(id))
}

/// `window.minimize [on|off|toggle]` (default toggle), resolved against the window's minimized state as read back:
/// `on` and `off` are idempotent where it is, and a request is sent anyway where it is not (Wayland). A window in full
/// screen is known not minimized (upstream checks idempotence before its full-screen guard), so restoring it is an ok
/// no-op and only a minimize is refused rather than reported done. Minimizing hands frontmost to a window still in view.
fn minimize(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let mode = ToggleMode::parse(args.mode.as_deref()).ok_or_else(|| {
        ControlError::rejected(format!(
            "invalid window minimize mode: {}",
            args.mode.as_deref().unwrap_or("toggle")
        ))
    })?;
    let id = open_window_id(core, target)?;
    let state = core.window_state(id).unwrap_or_default();
    let minimized = if state.fullscreen { Some(false) } else { state.minimized };
    let desired = mode.desired_value(minimized.unwrap_or(false));
    if minimized != Some(desired) {
        if state.fullscreen {
            return Err(ControlError::rejected(
                "cannot minimize a full-screen window — window.fullscreen it first",
            ));
        }
        core.request_window(WindowRequest::Minimize {
            window: id,
            minimized: desired,
        });
    }
    if desired {
        hand_off_frontmost(core, id);
    }
    Ok(with_id(id))
}

/// After parking the frontmost window, frontmost moves to the first other open window not read back as minimized
/// (upstream's `handOffFrontmost`), so untargeted commands stop routing into a window out of view. With none, the
/// pointer stays.
fn hand_off_frontmost(core: &mut Core, from: Uuid) {
    if core.library().frontmost_window_id != Some(from) {
        return;
    }
    let in_view = |id: &Uuid| core.window_state(*id).is_none_or(|state| state.minimized != Some(true));
    let next = core
        .library()
        .open_ids()
        .into_iter()
        .find(|id| *id != from && in_view(id));
    if let Some(next) = next {
        core.take_frontmost(next);
    }
}
