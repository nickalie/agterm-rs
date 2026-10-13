//! `window.*`: today `window.list` (upstream's `windowList` in `agterm/Control/ControlServer+WindowCommands.swift`,
//! projected as `WindowLibrary.controlWindowNodes`). Upstream answers it from a cache off the main actor; here it is
//! computed on dispatch.

use agterm_proto::command::WindowCmd;
use agterm_proto::{Command, ControlResult, ControlWindowNode};

use super::Outcome;
use crate::Core;
use crate::error::ControlError;
use crate::pane_env::uuid_string;

/// Answers a `window.*` command; those not implemented yet are refused by name.
pub(crate) fn dispatch(core: &Core, cmd: WindowCmd) -> Outcome {
    match cmd {
        WindowCmd::List => Ok(ControlResult::windows(window_nodes(core))),
        other => Err(ControlError::NotSupported(Command::Window(other))),
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
                minimized: live.map(|state| state.minimized),
            }
        })
        .collect()
}
