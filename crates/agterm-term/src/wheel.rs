//! The mouse wheel, routed as Ghostty's `Surface.scrollCallback` routes it (`GHOSTTY_REV`), once the frontend has
//! turned its scroll deltas into whole steps (Ghostty accumulates precise scrolling per cell and applies
//! `mouse-scroll-multiplier` first):
//!
//! 1. on the alternate screen, with no mouse tracking and alternate scroll (mode 1007, on by default) set, each row
//!    step is a cursor key (`ESC [ A`/`B`, `ESC O A`/`B` in application cursor mode) — `less` and `man` scroll;
//! 2. while mouse reporting is on, each step is reported as a press of button 4/5 (up/down) or 6/7 (left/right) at
//!    the pointer, the viewport untouched;
//! 3. otherwise the viewport scrolls by the row steps.
//!
//! As in Ghostty, both program routes clear the selection (one could have been made with the reporting override).

use agterm_core::ghostty_config::binds::Mods;
use agterm_ghostty_vt as vt;

use crate::PaneError;
use crate::input::{MouseAction, MouseButton, MouseInput};
use crate::pane::{Pane, Vt, lock};

/// A wheel turn in whole steps; positive is up (rows) and right (columns), as Ghostty counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WheelInput {
    pub rows: isize,
    pub columns: isize,
    pub mods: Mods,
    /// The pointer in surface pixels from the pane's top left, padding included (where a report says it is).
    pub x: f32,
    pub y: f32,
}

/// Where a wheel turn went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wheel {
    /// Reported to the program as wheel buttons.
    Reported,
    /// Sent to the program as cursor keys (alternate scroll).
    CursorKeys,
    /// The viewport scrolled.
    Scrolled,
    /// Nothing to do (no steps the chosen route uses).
    Nothing,
}

impl Pane {
    /// Route a wheel turn (module docs).
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when a report cannot be encoded; [`PaneError::Backpressure`] or [`PaneError::Closed`]
    /// from the input queue.
    pub fn wheel(&self, input: &WheelInput) -> Result<Wheel, PaneError> {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let routed = self.route(&mut vt, input);
        let repaint = matches!(routed, Ok(Wheel::Scrolled)) || vt.take_cleared_selection();
        drop(vt);
        if repaint {
            shared.wake();
        }
        routed
    }

    /// The routing under the VT lock.
    fn route(&self, vt: &mut Vt, input: &WheelInput) -> Result<Wheel, PaneError> {
        let alternate_scroll =
            vt.terminal.alternate_screen() && !vt.terminal.mouse_tracking() && vt.terminal.mode(vt::Mode::dec(1007));
        if alternate_scroll {
            if input.rows == 0 {
                return Ok(Wheel::Nothing);
            }
            self.clear_selection_held(vt);
            let key: &[u8] = match (vt.terminal.mode(vt::Mode::dec(1)), input.rows > 0) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1bOB",
                (false, true) => b"\x1b[A",
                (false, false) => b"\x1b[B",
            };
            self.queue(&key.repeat(input.rows.unsigned_abs()))?;
            return Ok(Wheel::CursorKeys);
        }
        if vt.input.reporting(&vt.terminal) {
            self.clear_selection_held(vt);
            let bytes = report(vt, input).map_err(PaneError::terminal)?;
            self.queue(&bytes)?;
            return Ok(Wheel::Reported);
        }
        if input.rows == 0 {
            return Ok(Wheel::Nothing);
        }
        vt.terminal.scroll_viewport(vt::Scroll::Rows(-input.rows));
        Ok(Wheel::Scrolled)
    }
}

/// The wheel buttons' press reports, one per step: rows first (4 up, 5 down), then columns (6 right, 7 left).
fn report(vt: &mut Vt, input: &WheelInput) -> Result<Vec<u8>, vt::Error> {
    let size = vt.size;
    let Vt {
        terminal, input: state, ..
    } = vt;
    let steps = [
        (input.rows, MouseButton::Four, MouseButton::Five),
        (input.columns, MouseButton::Six, MouseButton::Seven),
    ];
    let mut bytes = Vec::new();
    for (delta, positive, negative) in steps {
        let button = if delta > 0 { positive } else { negative };
        let press = MouseInput {
            action: MouseAction::Press,
            button: Some(button),
            mods: input.mods,
            x: input.x,
            y: input.y,
        };
        for _ in 0..delta.unsigned_abs() {
            bytes.extend(state.encode_mouse(terminal, size, &press)?);
        }
    }
    Ok(bytes)
}
