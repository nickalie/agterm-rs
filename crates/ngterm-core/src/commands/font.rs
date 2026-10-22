//! `font.inc`, `font.dec` and `font.reset`: one libghostty font binding action on a pane of a session (upstream's
//! `ControlDispatcher.dispatchFont` and `ControlServer+SurfaceIO.font`), and the cell-size reports that persist the main
//! pane's size (`GhosttySurfaceView.reportFontSize`, `agtermApp.persistFontSize`).
//!
//! The live size is read back per pane from the terminal (`tree.fontSize`, `splitFontSize`, `scratchFontSize`); only the
//! pane in the main role persists its size as the session's `fontSize`, which a new terminal of the session starts at.
//! A pane under an HTML page steps the app-wide page zoom instead, as the keys do.

use ngterm_proto::Request;
use ngterm_proto::command::FontCmd;
use uuid::Uuid;

use super::surface::{existing_pane, not_realized, run_binding};
use super::{Outcome, args_of, parse_surface_pane, session_store, with_id};
use crate::Core;
use crate::html::zoom;
use crate::model::StatusPane;

impl Core {
    /// A terminal of session `session` reported its live font size after a cell-size change (a font key, a font
    /// command, a DPI change): persisted, with a debounced save, as the session's font size when the pane carrying
    /// `token` is in the main role now (a promoted split survivor included); the split and the scratch stay live-only.
    /// The frontend drops the reports of a terminal under a dashboard font override, whose size is transient. Whether
    /// the session changed; commit afterwards when it did.
    pub fn report_font_size(&mut self, session: Uuid, token: &str, size: f64) -> bool {
        if !size.is_finite() || size <= 0.0 {
            return false;
        }
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        let Some(found) = store.session(session) else {
            return false;
        };
        if found.pane_role_for_token(token) != Some(StatusPane::Left) || found.font_size == Some(size) {
            return false;
        }
        store.set_font_size(session, size);
        true
    }
}

/// The libghostty binding action of a font command.
fn action(cmd: FontCmd) -> &'static str {
    match cmd {
        FontCmd::Inc => "increase_font_size:1",
        FontCmd::Dec => "decrease_font_size:1",
        FontCmd::Reset => "reset_font_size",
    }
}

/// `font.inc|dec|reset [--pane]`: the action on the addressed pane, the main one by default (a promoted split survivor
/// included); a missing split or scratch is refused before anything else, an unrealized terminal after the page check.
pub(crate) fn dispatch(core: &mut Core, cmd: FontCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let requested = parse_surface_pane(args.pane.as_deref())?;
    let action = action(cmd);
    let (store, id) = session_store(core, request.target.as_deref(), args.window.as_deref())?;
    let session = store.session_mut(id).ok_or_else(not_realized)?;
    let pane = match requested {
        None | Some(StatusPane::Left) => session.addressable_pane(),
        Some(pane) => existing_pane(session, pane)?,
    };
    // after the pane checks, so a missing pane keeps its error, and before the realized one: a page zooms without
    // its terminal
    if session.html_hides_terminal(requested) {
        step_html_zoom(core, action);
        return Ok(with_id(id));
    }
    if !run_binding(session, pane, action) {
        // a dormant pane spawns now, for the next call (upstream's `expediteSpawn`)
        core.wake_session(id);
        return Err(not_realized());
    }
    Ok(with_id(id))
}

/// Moves every page's zoom by the font action and saves it (upstream's `SettingsModel.stepHtmlOverlayZoom`); actual
/// size is stored as unset.
fn step_html_zoom(core: &mut Core, action: &str) {
    let Some(next) = zoom::applying(action, core.html_zoom()) else {
        return;
    };
    let stored = ((next - 1.0).abs() > f64::EPSILON).then_some(next);
    core.update_settings(|settings| settings.html_overlay_zoom = stored);
}
