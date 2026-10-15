//! Ghostty's selection gestures (`selection.h`, `GhosttySelectionGesture`): the state machine that turns pointer
//! presses, drags and releases into selections — single, double and triple clicks by time and distance, word and line
//! granular drags, the half-cell threshold before a drag selects, autoscroll past the top or bottom edge — exactly as
//! Ghostty's own surface does it (`Surface.mouseButtonCallback`, `cursorPosCallback`).
//!
//! The gesture keeps tracked references into its terminal, so every call takes that terminal `&mut` (serialized with
//! everything else on it). The selection an event produces is returned as points, never installed: the caller decides
//! (Ghostty installs a press's and a drag's result, clears the selection on a single click that made none).

use crate::selection::{Point, PointTag, Selection};
use crate::{Error, Terminal, ffi};

/// What a click of a given count selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GestureBehavior {
    /// Cell-granular drag selection.
    Cell,
    /// The word under the press, word-granular drags.
    Word,
    /// The line under the press, line-granular drags.
    Line,
    /// The command output under the press (semantic prompts).
    Output,
}

impl GestureBehavior {
    fn raw(self) -> ffi::GhosttySelectionGestureBehavior {
        match self {
            Self::Cell => ffi::GHOSTTY_SELECTION_GESTURE_BEHAVIOR_CELL,
            Self::Word => ffi::GHOSTTY_SELECTION_GESTURE_BEHAVIOR_WORD,
            Self::Line => ffi::GHOSTTY_SELECTION_GESTURE_BEHAVIOR_LINE,
            Self::Output => ffi::GHOSTTY_SELECTION_GESTURE_BEHAVIOR_OUTPUT,
        }
    }
}

/// Where a drag past an edge wants the viewport to scroll.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Autoscroll {
    #[default]
    None,
    Up,
    Down,
}

/// The grid as the pointer sees it (surface pixels).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct GestureGeometry {
    pub columns: u32,
    pub cell_width: u32,
    pub padding_left: u32,
    pub screen_height: u32,
}

impl GestureGeometry {
    fn raw(self) -> ffi::GhosttySelectionGestureGeometry {
        ffi::GhosttySelectionGestureGeometry {
            columns: self.columns.max(1),
            cell_width: self.cell_width.max(1),
            padding_left: self.padding_left,
            screen_height: self.screen_height.max(1),
        }
    }
}

/// A left-button press.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GesturePress {
    /// The cell under the pointer.
    pub at: Point,
    /// The pointer in surface pixels.
    pub x: f64,
    pub y: f64,
    /// A monotonic time stamp of the press, in nanoseconds.
    pub time_ns: u64,
    /// How long after a press the next one still counts as a repeat (Ghostty's `click-repeat-interval`).
    pub repeat_interval_ns: u64,
    /// How far (surface pixels) the next press may land and still count as a repeat (Ghostty: one cell's width).
    pub repeat_distance: f64,
    /// What a single, a double and a triple click select.
    pub behaviors: [GestureBehavior; 3],
}

/// The pointer moved with the left button held.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureDrag {
    pub at: Point,
    pub x: f64,
    pub y: f64,
    pub rectangle: bool,
    pub geometry: GestureGeometry,
}

/// One autoscroll step while a drag rests past an edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureTick {
    /// The viewport cell under the pointer (clamped to the grid).
    pub viewport: (u16, u32),
    pub x: f64,
    pub y: f64,
    pub rectangle: bool,
    pub geometry: GestureGeometry,
}

/// A pointer event for the gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureInput {
    Press(GesturePress),
    Drag(GestureDrag),
    /// The left button went up over this cell (`None`: off the grid).
    Release(Option<Point>),
    Tick(GestureTick),
}

/// The gesture's state after an event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct GestureState {
    /// 0 when no click sequence is active, else 1 to 3.
    pub click_count: u8,
    /// The current (or last) press moved off its cell.
    pub dragged: bool,
    pub autoscroll: Autoscroll,
}

/// One pointer's selection gesture: an owned handle, `Send` and not `Sync` (crate docs).
///
/// Free it with [`SelectionGesture::close`] against the terminal it was used with. Dropping it unclosed leaks the
/// handle rather than touch a terminal another thread may be writing.
#[derive(Debug)]
pub struct SelectionGesture {
    raw: ffi::GhosttySelectionGesture,
}

// SAFETY: the gesture is a plain library allocation with no thread affinity; `&mut self` serializes the calls on it
// and every call touching its terminal takes the terminal `&mut` too.
unsafe impl Send for SelectionGesture {}

/// An event object for one call, freed with it.
struct Event(ffi::GhosttySelectionGestureEvent);

impl Event {
    fn new(kind: ffi::GhosttySelectionGestureEventType) -> Result<Self, Error> {
        let mut raw: ffi::GhosttySelectionGestureEvent = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer; the type is one of the header's constants.
        Error::check(unsafe { ffi::ghostty_selection_gesture_event_new(std::ptr::null(), &raw mut raw, kind) })?;
        if raw.is_null() {
            return Err(Error::OutOfMemory);
        }
        Ok(Self(raw))
    }

    /// Sets `option` to `value`, whose type the header documents for that option.
    fn set<T>(&self, option: ffi::GhosttySelectionGestureEventOption, value: &T) -> Result<(), Error> {
        // SAFETY: the event is live; the value outlives the call, which copies it.
        Error::check(unsafe {
            ffi::ghostty_selection_gesture_event_set(self.0, option, std::ptr::from_ref(value).cast())
        })
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the event is live and dropped once.
        unsafe { ffi::ghostty_selection_gesture_event_free(self.0) };
    }
}

impl SelectionGesture {
    /// A gesture with no active click sequence.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when the allocation fails.
    pub fn new() -> Result<Self, Error> {
        let mut raw: ffi::GhosttySelectionGesture = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer.
        Error::check(unsafe { ffi::ghostty_selection_gesture_new(std::ptr::null(), &raw mut raw) })?;
        if raw.is_null() {
            return Err(Error::OutOfMemory);
        }
        Ok(Self { raw })
    }

    /// Free the gesture, releasing what it tracks in `terminal` (the one it was last used with).
    pub fn close(self, terminal: &mut Terminal) {
        // SAFETY: the gesture is live and consumed; its terminal is borrowed mutably, serializing the release.
        unsafe { ffi::ghostty_selection_gesture_free(self.raw, terminal.as_raw()) };
    }

    /// Cancel the active click sequence (Ghostty does when the program takes the mouse).
    pub fn reset(&mut self, terminal: &mut Terminal) {
        // SAFETY: the gesture is live; its terminal is borrowed mutably.
        unsafe { ffi::ghostty_selection_gesture_reset(self.raw, terminal.as_raw()) };
    }

    /// Apply one pointer event; the selection it produces, in screen coordinates (`None` when it produces none: a
    /// release, a single click, a drag still inside its first cell).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a point outside its space, or the library's result.
    pub fn apply(&mut self, terminal: &mut Terminal, input: &GestureInput) -> Result<Option<Selection>, Error> {
        self.apply_with(terminal, input, None)
    }

    /// [`SelectionGesture::apply`] with `boundaries` (Unicode scalar values, Ghostty's `selection-word-chars`) as the
    /// word boundaries of word clicks and word-granular drags; `None` keeps Ghostty's defaults.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a point outside its space, or the library's result.
    pub fn apply_with(
        &mut self,
        terminal: &mut Terminal,
        input: &GestureInput,
        boundaries: Option<&[u32]>,
    ) -> Result<Option<Selection>, Error> {
        let event = match input {
            GestureInput::Press(press) => press_event(terminal, press)?,
            GestureInput::Drag(drag) => drag_event(terminal, drag)?,
            GestureInput::Release(at) => release_event(terminal, *at)?,
            GestureInput::Tick(tick) => tick_event(tick)?,
        };
        // releases take no boundaries (the option is not valid for them)
        if let Some(boundaries) = boundaries
            && !matches!(input, GestureInput::Release(_))
        {
            let codepoints = ffi::GhosttyCodepoints {
                ptr: boundaries.as_ptr(),
                len: boundaries.len(),
            };
            event.set(
                ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_WORD_BOUNDARY_CODEPOINTS,
                &codepoints,
            )?;
        }
        let mut selection = crate::selection::empty_selection();
        let raw_terminal = terminal.as_raw();
        // SAFETY: gesture, terminal (borrowed mutably) and event are live; `selection` is a sized out-struct.
        let result =
            unsafe { ffi::ghostty_selection_gesture_event(self.raw, raw_terminal, event.0, &raw mut selection) };
        match Error::check(result) {
            Ok(()) => {}
            Err(Error::NoValue) => return Ok(None),
            Err(err) => return Err(err),
        }
        // read back before the terminal is let go: the snapshot dies on its next mutation
        let start = terminal.point_of(&selection.start, PointTag::Screen);
        let end = terminal.point_of(&selection.end, PointTag::Screen);
        Ok(start.zip(end).map(|(start, end)| Selection {
            start,
            end,
            rectangle: selection.rectangle,
        }))
    }

    /// The gesture's click count, drag flag and autoscroll request.
    #[must_use]
    pub fn state(&self, terminal: &Terminal) -> GestureState {
        let mut count: u8 = 0;
        let mut dragged = false;
        let mut autoscroll: ffi::GhosttySelectionGestureAutoscroll = ffi::GHOSTTY_SELECTION_GESTURE_AUTOSCROLL_NONE;
        let raw_terminal = terminal.raw_shared();
        // SAFETY: gesture and terminal are live (the terminal borrowed); `CLICK_COUNT` writes a `u8`.
        unsafe {
            ffi::ghostty_selection_gesture_get(
                self.raw,
                raw_terminal,
                ffi::GHOSTTY_SELECTION_GESTURE_DATA_CLICK_COUNT,
                (&raw mut count).cast(),
            )
        };
        // SAFETY: as above; `DRAGGED` writes a `bool`.
        unsafe {
            ffi::ghostty_selection_gesture_get(
                self.raw,
                raw_terminal,
                ffi::GHOSTTY_SELECTION_GESTURE_DATA_DRAGGED,
                (&raw mut dragged).cast(),
            )
        };
        // SAFETY: as above; `AUTOSCROLL` writes a `GhosttySelectionGestureAutoscroll`.
        unsafe {
            ffi::ghostty_selection_gesture_get(
                self.raw,
                raw_terminal,
                ffi::GHOSTTY_SELECTION_GESTURE_DATA_AUTOSCROLL,
                (&raw mut autoscroll).cast(),
            )
        };
        GestureState {
            click_count: count,
            dragged,
            autoscroll: match autoscroll {
                ffi::GHOSTTY_SELECTION_GESTURE_AUTOSCROLL_UP => Autoscroll::Up,
                ffi::GHOSTTY_SELECTION_GESTURE_AUTOSCROLL_DOWN => Autoscroll::Down,
                _ => Autoscroll::None,
            },
        }
    }
}

fn position(x: f64, y: f64) -> ffi::GhosttySurfacePosition {
    ffi::GhosttySurfacePosition { x, y }
}

fn press_event(terminal: &Terminal, press: &GesturePress) -> Result<Event, Error> {
    let event = Event::new(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_PRESS)?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF,
        &terminal.grid_ref(press.at)?,
    )?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION,
        &position(press.x, press.y),
    )?;
    event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_TIME_NS, &press.time_ns)?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REPEAT_INTERVAL_NS,
        &press.repeat_interval_ns,
    )?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REPEAT_DISTANCE,
        &press.repeat_distance,
    )?;
    let [single, double, triple] = press.behaviors;
    let behaviors = ffi::GhosttySelectionGestureBehaviors {
        single_click: single.raw(),
        double_click: double.raw(),
        triple_click: triple.raw(),
    };
    event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_BEHAVIORS, &behaviors)?;
    Ok(event)
}

fn drag_event(terminal: &Terminal, drag: &GestureDrag) -> Result<Event, Error> {
    let event = Event::new(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_DRAG)?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF,
        &terminal.grid_ref(drag.at)?,
    )?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION,
        &position(drag.x, drag.y),
    )?;
    event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_RECTANGLE, &drag.rectangle)?;
    event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_GEOMETRY, &drag.geometry.raw())?;
    Ok(event)
}

fn release_event(terminal: &Terminal, at: Option<Point>) -> Result<Event, Error> {
    let event = Event::new(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_RELEASE)?;
    if let Some(grid_ref) = at.and_then(|at| terminal.grid_ref(at).ok()) {
        event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF, &grid_ref)?;
    }
    Ok(event)
}

fn tick_event(tick: &GestureTick) -> Result<Event, Error> {
    let event = Event::new(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_AUTOSCROLL_TICK)?;
    let (x, y) = tick.viewport;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_VIEWPORT,
        &ffi::GhosttyPointCoordinate { x, y },
    )?;
    event.set(
        ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION,
        &position(tick.x, tick.y),
    )?;
    event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_RECTANGLE, &tick.rectangle)?;
    event.set(ffi::GHOSTTY_SELECTION_GESTURE_EVENT_OPT_GEOMETRY, &tick.geometry.raw())?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    const GEOMETRY: GestureGeometry = GestureGeometry {
        columns: 20,
        cell_width: 10,
        padding_left: 0,
        screen_height: 60,
    };

    fn viewport(x: u16, y: u32) -> Point {
        Point::new(PointTag::Viewport, x, y)
    }

    fn press(x: u16, y: u32, time_ns: u64) -> GestureInput {
        GestureInput::Press(GesturePress {
            at: viewport(x, y),
            x: f64::from(x) * 10.0 + 2.0,
            y: f64::from(y) * 20.0 + 5.0,
            time_ns,
            repeat_interval_ns: 500 * MS,
            repeat_distance: 10.0,
            behaviors: [GestureBehavior::Cell, GestureBehavior::Word, GestureBehavior::Line],
        })
    }

    fn drag(x: u16, y: u32) -> GestureInput {
        GestureInput::Drag(GestureDrag {
            at: viewport(x, y),
            x: f64::from(x) * 10.0 + 8.0,
            y: f64::from(y) * 20.0 + 5.0,
            rectangle: false,
            geometry: GEOMETRY,
        })
    }

    fn xs(selection: Option<Selection>) -> Option<(u16, u16)> {
        selection.map(|s| (s.start.x.min(s.end.x), s.start.x.max(s.end.x)))
    }

    #[test]
    fn clicks_count_by_time_and_select_words_and_lines() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"one two three");
        let mut gesture = SelectionGesture::new().unwrap();
        assert_eq!(gesture.apply(&mut terminal, &press(5, 0, 0)).unwrap(), None);
        assert_eq!(gesture.state(&terminal).click_count, 1);
        gesture
            .apply(&mut terminal, &GestureInput::Release(Some(viewport(5, 0))))
            .unwrap();
        let word = gesture.apply(&mut terminal, &press(5, 0, 100 * MS)).unwrap();
        assert_eq!(gesture.state(&terminal).click_count, 2);
        assert_eq!(xs(word), Some((4, 6)));
        gesture
            .apply(&mut terminal, &GestureInput::Release(Some(viewport(5, 0))))
            .unwrap();
        let line = gesture.apply(&mut terminal, &press(5, 0, 200 * MS)).unwrap();
        assert_eq!(gesture.state(&terminal).click_count, 3);
        assert_eq!(xs(line), Some((0, 12)));
        // a press long after starts over
        gesture.apply(&mut terminal, &GestureInput::Release(None)).unwrap();
        assert_eq!(gesture.apply(&mut terminal, &press(5, 0, 2_000 * MS)).unwrap(), None);
        assert_eq!(gesture.state(&terminal).click_count, 1);
        gesture.close(&mut terminal);
    }

    #[test]
    fn word_clicks_take_the_given_boundaries() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"one;two three");
        let mut gesture = SelectionGesture::new().unwrap();
        gesture.apply(&mut terminal, &press(1, 0, 0)).unwrap();
        gesture
            .apply(&mut terminal, &GestureInput::Release(Some(viewport(1, 0))))
            .unwrap();
        // `;` is a default boundary; with only a space as one, `one;two` is a word
        let boundaries = [0, u32::from(' ')];
        let word = gesture
            .apply_with(&mut terminal, &press(1, 0, 100 * MS), Some(&boundaries))
            .unwrap();
        assert_eq!(xs(word), Some((0, 6)));
        assert!(
            terminal
                .select_word_with(viewport(11, 0), Some(&[0, u32::from('h')]))
                .unwrap()
        );
        let selection = terminal.selection(PointTag::Viewport).unwrap();
        assert_eq!(
            (selection.start.x, selection.end.x),
            (10, 12),
            "`h` and the line's end bound `ree`"
        );
        gesture.close(&mut terminal);
    }

    #[test]
    fn a_drag_selects_cells_and_marks_the_gesture_dragged() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"hello world");
        let mut gesture = SelectionGesture::new().unwrap();
        gesture.apply(&mut terminal, &press(1, 0, 0)).unwrap();
        let dragged = gesture.apply(&mut terminal, &drag(7, 0)).unwrap();
        assert_eq!(xs(dragged), Some((1, 7)));
        gesture
            .apply(&mut terminal, &GestureInput::Release(Some(viewport(7, 0))))
            .unwrap();
        assert!(gesture.state(&terminal).dragged);
        gesture.reset(&mut terminal);
        assert_eq!(gesture.state(&terminal).click_count, 0);
        gesture.close(&mut terminal);
    }
}
