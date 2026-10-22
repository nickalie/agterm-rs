//! The pointer over a pane: Ghostty's selection gesture (clicks counted by time and distance, word and line clicks,
//! drags past the half-cell threshold, autoscroll past an edge) and what lies under the pointer (its cell, an OSC 8
//! hyperlink).
//!
//! Positions are surface pixels from the pane's top left, padding included (as [`crate::MouseInput`]). The gesture
//! lives under the VT lock with the terminal it tracks; its selections are installed as Ghostty's surface installs
//! them (`Surface.mouseButtonCallback`, `cursorPosCallback`, `selectionScrollTick`): a press's result, or a cleared
//! selection after a single click that made none; every drag's result, none included; an autoscroll tick's while a
//! click sequence is live.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ngterm_ghostty_vt as vt;

use crate::PaneError;
use crate::pane::{Pane, Vt, lock};
use crate::selection::GridPoint;

/// Ghostty's `click-repeat-interval` when it is 0 (the platform default, 500 ms off macOS).
pub const CLICK_REPEAT_INTERVAL: Duration = Duration::from_millis(500);

/// A pointer event for the selection gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureEvent {
    /// The left button went down at (`x`, `y`) at `time`; a triple click selects the command output instead of the
    /// line when `output` (Ghostty: Ctrl or Super held); a press within `repeat_interval` of the last one (and within
    /// a cell's width) counts as a repeat (`click-repeat-interval`).
    Press {
        x: f32,
        y: f32,
        time: Instant,
        output: bool,
        repeat_interval: Duration,
    },
    /// The pointer moved with the left button down; `rectangle` selects a block (Ghostty: Ctrl+Alt on Linux).
    Drag { x: f32, y: f32, rectangle: bool },
    /// The left button went up at (`x`, `y`).
    Release { x: f32, y: f32 },
    /// One autoscroll step (every 15 ms while [`Gesture::autoscroll`] asks for one), the pointer at (`x`, `y`).
    Tick { x: f32, y: f32, rectangle: bool },
}

/// Where a drag past an edge wants the viewport to scroll.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Autoscroll {
    #[default]
    None,
    Up,
    Down,
}

/// The gesture after an event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Gesture {
    /// 0 with no live click sequence, else 1 to 3.
    pub click_count: u8,
    /// The current (or last) press moved off its cell.
    pub dragged: bool,
    pub autoscroll: Autoscroll,
}

/// The time stamps' origin: monotonic nanoseconds since the first gesture of the process.
fn time_ns(time: Instant) -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(|| time.checked_sub(Duration::from_secs(1)).unwrap_or(time));
    u64::try_from(time.saturating_duration_since(epoch).as_nanos()).unwrap_or(u64::MAX)
}

impl Vt {
    /// The viewport cell at a surface position, clamped to the grid (Ghostty's `posToViewport`).
    pub(crate) fn cell_at(&self, x: f32, y: f32) -> (u16, u32) {
        let size = self.size;
        let padding = self.input.padding();
        let along = |pos: f32, start: u32, cell: u32, count: u32| -> u32 {
            let offset = f64::from(pos) - f64::from(start);
            if offset <= 0.0 || cell == 0 {
                return 0;
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a positive cell index"
            )]
            let index = (offset / f64::from(cell)) as u32;
            index.min(count.saturating_sub(1))
        };
        let col = along(x, padding.left, size.cell_width, u32::from(size.cols));
        let row = along(y, padding.top, size.cell_height, u32::from(size.rows));
        (u16::try_from(col).unwrap_or(u16::MAX), row)
    }

    fn geometry(&self) -> vt::GestureGeometry {
        let size = self.size;
        let padding = self.input.padding();
        vt::GestureGeometry {
            columns: u32::from(size.cols),
            cell_width: size.cell_width,
            padding_left: padding.left,
            screen_height: u32::from(size.rows) * size.cell_height + padding.top + padding.bottom,
        }
    }

    fn engine_event(&self, event: GestureEvent) -> vt::GestureInput {
        let viewport = |x: f32, y: f32| {
            let (col, row) = self.cell_at(x, y);
            vt::Point::new(vt::PointTag::Viewport, col, row)
        };
        match event {
            GestureEvent::Press {
                x,
                y,
                time,
                output,
                repeat_interval,
            } => vt::GestureInput::Press(vt::GesturePress {
                at: viewport(x, y),
                x: f64::from(x),
                y: f64::from(y),
                time_ns: time_ns(time),
                repeat_interval_ns: u64::try_from(repeat_interval.as_nanos()).unwrap_or(u64::MAX),
                repeat_distance: f64::from(self.size.cell_width),
                behaviors: [
                    vt::GestureBehavior::Cell,
                    vt::GestureBehavior::Word,
                    if output {
                        vt::GestureBehavior::Output
                    } else {
                        vt::GestureBehavior::Line
                    },
                ],
            }),
            GestureEvent::Drag { x, y, rectangle } => vt::GestureInput::Drag(vt::GestureDrag {
                at: viewport(x, y),
                x: f64::from(x),
                y: f64::from(y),
                rectangle,
                geometry: self.geometry(),
            }),
            GestureEvent::Release { x, y } => vt::GestureInput::Release(Some(viewport(x, y))),
            GestureEvent::Tick { x, y, rectangle } => vt::GestureInput::Tick(vt::GestureTick {
                viewport: self.cell_at(x, y),
                x: f64::from(x),
                y: f64::from(y),
                rectangle,
                geometry: self.geometry(),
            }),
        }
    }
}

fn gesture_state(state: vt::GestureState) -> Gesture {
    Gesture {
        click_count: state.click_count,
        dragged: state.dragged,
        autoscroll: match state.autoscroll {
            vt::Autoscroll::None => Autoscroll::None,
            vt::Autoscroll::Up => Autoscroll::Up,
            vt::Autoscroll::Down => Autoscroll::Down,
        },
    }
}

impl Pane {
    /// Apply a pointer event to the selection gesture, installing the selection it makes (module docs).
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library refuses the event.
    pub fn gesture(&self, event: GestureEvent) -> Result<Gesture, PaneError> {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let input = vt.engine_event(event);
        if vt.gesture.is_none() {
            vt.gesture = Some(vt::SelectionGesture::new().map_err(PaneError::terminal)?);
        }
        let Vt {
            terminal,
            gesture,
            word_boundaries,
            ..
        } = &mut *vt;
        let Some(gesture) = gesture.as_mut() else {
            return Ok(Gesture::default());
        };
        let selection = gesture
            .apply(terminal, &input, word_boundaries.as_deref())
            .map_err(PaneError::terminal)?;
        let state = gesture.state(terminal);
        let had = terminal.selection(vt::PointTag::Screen).is_some();
        let install = match event {
            GestureEvent::Press { .. } => selection.is_some() || (state.click_count == 1 && had),
            GestureEvent::Drag { .. } => selection.is_some() || had,
            GestureEvent::Tick { .. } => state.click_count != 0 && (selection.is_some() || had),
            GestureEvent::Release { .. } => false,
        };
        if install {
            let installed = match selection {
                Some(selection) => terminal.set_selection(selection.start, selection.end, selection.rectangle),
                None => terminal.clear_selection(),
            };
            installed.map_err(PaneError::terminal)?;
            // VT → capture
            shared.touch_capture();
        }
        drop(vt);
        if install || matches!(event, GestureEvent::Tick { .. }) {
            shared.wake();
        }
        Ok(gesture_state(state))
    }

    /// The gesture's state without an event.
    #[must_use]
    pub fn gesture_state(&self) -> Gesture {
        let vt = lock(&self.shared().vt);
        vt.gesture
            .as_ref()
            .map_or_else(Gesture::default, |gesture| gesture_state(gesture.state(&vt.terminal)))
    }

    /// Cancel the active click sequence (the program took the mouse).
    pub fn reset_gesture(&self) {
        let mut vt = lock(&self.shared().vt);
        let Vt { terminal, gesture, .. } = &mut *vt;
        if let Some(gesture) = gesture.as_mut() {
            gesture.reset(terminal);
        }
    }

    /// The viewport cell under a surface position, clamped to the grid.
    #[must_use]
    pub fn cell_at(&self, x: f32, y: f32) -> GridPoint {
        let (col, row) = lock(&self.shared().vt).cell_at(x, y);
        GridPoint::viewport(col, row)
    }

    /// The URI of the OSC 8 hyperlink on the cell at `at`; `None` without one (or for a point outside its space).
    #[must_use]
    pub fn hyperlink(&self, at: GridPoint) -> Option<String> {
        lock(&self.shared().vt)
            .terminal
            .hyperlink_uri(at.engine())
            .map_err(|err| tracing::debug!(%err, "reading a hyperlink failed"))
            .ok()
            .flatten()
    }
}
