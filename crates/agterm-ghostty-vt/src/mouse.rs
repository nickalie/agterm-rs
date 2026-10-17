//! Mouse reporting (`mouse/encoder.h`, `mouse/event.h`): a pointer event in surface pixels in, the report the running
//! program asked for out (X10, normal, button or any-event tracking; X10, UTF-8, SGR, urxvt or SGR-pixel format).

use crate::key::{KeyMods, encode_with};
use crate::{Error, Handle, Mode, Terminal, ffi};

/// The modes the encoder's tracking mode and format come from: X10 (9), normal (1000), button (1002) and any-event
/// (1003) tracking; UTF-8 (1005), SGR (1006), urxvt (1015) and SGR-pixel (1016) format.
const MOUSE_MODES: [u16; 8] = [9, 1000, 1002, 1003, 1005, 1006, 1015, 1016];

/// A pointer event's kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MouseAction {
    #[default]
    Press,
    Release,
    Motion,
}

/// A mouse button; wheel steps are buttons 4–7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Eleven,
}

impl MouseButton {
    fn raw(self) -> ffi::GhosttyMouseButton {
        match self {
            Self::Left => ffi::GHOSTTY_MOUSE_BUTTON_LEFT,
            Self::Right => ffi::GHOSTTY_MOUSE_BUTTON_RIGHT,
            Self::Middle => ffi::GHOSTTY_MOUSE_BUTTON_MIDDLE,
            Self::Four => ffi::GHOSTTY_MOUSE_BUTTON_FOUR,
            Self::Five => ffi::GHOSTTY_MOUSE_BUTTON_FIVE,
            Self::Six => ffi::GHOSTTY_MOUSE_BUTTON_SIX,
            Self::Seven => ffi::GHOSTTY_MOUSE_BUTTON_SEVEN,
            Self::Eight => ffi::GHOSTTY_MOUSE_BUTTON_EIGHT,
            Self::Nine => ffi::GHOSTTY_MOUSE_BUTTON_NINE,
            Self::Ten => ffi::GHOSTTY_MOUSE_BUTTON_TEN,
            Self::Eleven => ffi::GHOSTTY_MOUSE_BUTTON_ELEVEN,
        }
    }
}

/// A pointer event; the position is in surface pixels (padding included), from the surface's top left.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MouseEvent {
    pub action: MouseAction,
    /// `None` for a motion with no button held.
    pub button: Option<MouseButton>,
    pub mods: KeyMods,
    pub x: f32,
    pub y: f32,
}

/// The rendered surface the positions are in: its size, the cell size and the padding around the grid, in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MouseGeometry {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

/// A mouse encoder with its reusable event: owned handles, `Send` and not `Sync` (crate docs). Motion is reported
/// once per cell, as Ghostty does.
///
/// Reading the terminal's mouse modes into the encoder, or a new geometry, forgets the last reported cell, so both
/// are applied only when they change: the modes when one of [`MOUSE_MODES`] flips (a program re-setting a mode that
/// is already set changes nothing here, where Ghostty's "last one set wins" could), the geometry when it differs.
#[derive(Debug)]
pub struct MouseEncoder {
    event: Handle<ffi::GhosttyMouseEventImpl>,
    encoder: Handle<ffi::GhosttyMouseEncoderImpl>,
    /// The mouse modes last read into the encoder, one bit each.
    modes: Option<u8>,
    geometry: Option<MouseGeometry>,
}

// SAFETY: both handles are plain library allocations with no thread affinity (crate docs); `&mut self` serializes
// every call on them.
unsafe impl Send for MouseEncoder {}

impl MouseEncoder {
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when an allocation fails.
    pub fn new() -> Result<Self, Error> {
        let mut encoder: ffi::GhosttyMouseEncoder = std::ptr::null_mut();
        // SAFETY: a null allocator selects the default one; `encoder` is a valid out-pointer.
        Error::check(unsafe { ffi::ghostty_mouse_encoder_new(std::ptr::null(), &raw mut encoder) })?;
        let encoder = Handle::new(encoder, ffi::ghostty_mouse_encoder_free)?;
        let mut event: ffi::GhosttyMouseEvent = std::ptr::null_mut();
        // SAFETY: as above.
        Error::check(unsafe { ffi::ghostty_mouse_event_new(std::ptr::null(), &raw mut event) })?;
        let event = Handle::new(event, ffi::ghostty_mouse_event_free)?;
        let track = true;
        // SAFETY: the encoder is live; `TRACK_LAST_CELL` reads a `bool` during the call.
        unsafe {
            ffi::ghostty_mouse_encoder_setopt(
                encoder.ptr(),
                ffi::GHOSTTY_MOUSE_ENCODER_OPT_TRACK_LAST_CELL,
                (&raw const track).cast(),
            );
        };
        Ok(Self {
            event,
            encoder,
            modes: None,
            geometry: None,
        })
    }

    /// The report `event` sends under `terminal`'s current mouse modes; empty when it reports nothing (tracking off,
    /// a motion the mode ignores or within the last reported cell). `any_button_pressed` says whether a button is
    /// held, which button-event tracking needs for motion.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] for a zero cell size, or the library's result when encoding fails.
    pub fn encode(
        &mut self,
        terminal: &mut Terminal,
        geometry: &MouseGeometry,
        any_button_pressed: bool,
        event: &MouseEvent,
    ) -> Result<Vec<u8>, Error> {
        if geometry.cell_width == 0 || geometry.cell_height == 0 {
            return Err(Error::InvalidValue);
        }
        self.configure(terminal, geometry, any_button_pressed);
        self.fill(event);
        encode_with(|buffer, len, written| {
            // SAFETY: both handles are live; `buffer` is valid for `len` bytes of writes and `written` is a valid
            // out-pointer.
            unsafe { ffi::ghostty_mouse_encoder_encode(self.encoder.ptr(), self.event.ptr(), buffer, len, written) }
        })
    }

    /// Forget the last reported cell, so the next motion reports even in the same cell (after a mode change).
    pub fn reset(&mut self) {
        // SAFETY: the encoder is live and owned by `self`.
        unsafe { ffi::ghostty_mouse_encoder_reset(self.encoder.ptr()) };
    }

    fn configure(&mut self, terminal: &mut Terminal, geometry: &MouseGeometry, any_button_pressed: bool) {
        let encoder = self.encoder.ptr();
        let modes = MOUSE_MODES.iter().enumerate().fold(0_u8, |bits, (bit, &mode)| {
            bits | (u8::from(terminal.mode(Mode::dec(mode))) << bit)
        });
        if self.modes != Some(modes) {
            self.modes = Some(modes);
            // SAFETY: the encoder is live; the `&mut` terminal borrow serializes the read of its modes.
            unsafe { ffi::ghostty_mouse_encoder_setopt_from_terminal(encoder, terminal.as_raw()) };
        }
        if self.geometry != Some(*geometry) {
            self.geometry = Some(*geometry);
            self.set_size(geometry);
        }
        // SAFETY: the encoder is live; `OPT_ANY_BUTTON_PRESSED` reads a `bool` during the call.
        unsafe {
            ffi::ghostty_mouse_encoder_setopt(
                encoder,
                ffi::GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED,
                (&raw const any_button_pressed).cast(),
            );
        };
    }

    fn set_size(&mut self, geometry: &MouseGeometry) {
        let size = ffi::GhosttyMouseEncoderSize {
            size: std::mem::size_of::<ffi::GhosttyMouseEncoderSize>(),
            screen_width: geometry.screen_width,
            screen_height: geometry.screen_height,
            cell_width: geometry.cell_width,
            cell_height: geometry.cell_height,
            padding_top: geometry.padding_top,
            padding_bottom: geometry.padding_bottom,
            padding_right: geometry.padding_right,
            padding_left: geometry.padding_left,
        };
        // SAFETY: the encoder is live; `OPT_SIZE` reads a `GhosttyMouseEncoderSize` during the call.
        unsafe {
            ffi::ghostty_mouse_encoder_setopt(
                self.encoder.ptr(),
                ffi::GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                (&raw const size).cast(),
            );
        };
    }

    fn fill(&mut self, event: &MouseEvent) {
        let raw = self.event.ptr();
        let action = match event.action {
            MouseAction::Press => ffi::GHOSTTY_MOUSE_ACTION_PRESS,
            MouseAction::Release => ffi::GHOSTTY_MOUSE_ACTION_RELEASE,
            MouseAction::Motion => ffi::GHOSTTY_MOUSE_ACTION_MOTION,
        };
        // SAFETY: the event is live and owned by `self`.
        unsafe { ffi::ghostty_mouse_event_set_action(raw, action) };
        match event.button {
            // SAFETY: as above; `raw()` yields only values of the enum.
            Some(button) => unsafe { ffi::ghostty_mouse_event_set_button(raw, button.raw()) },
            // SAFETY: as above.
            None => unsafe { ffi::ghostty_mouse_event_clear_button(raw) },
        }
        // SAFETY: as above.
        unsafe { ffi::ghostty_mouse_event_set_mods(raw, event.mods.bits()) };
        let position = ffi::GhosttyMousePosition { x: event.x, y: event.y };
        // SAFETY: as above.
        unsafe { ffi::ghostty_mouse_event_set_position(raw, position) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEOMETRY: MouseGeometry = MouseGeometry {
        screen_width: 800,
        screen_height: 480,
        cell_width: 10,
        cell_height: 20,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    };

    fn press(x: f32, y: f32) -> MouseEvent {
        MouseEvent {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            mods: KeyMods::empty(),
            x,
            y,
        }
    }

    #[test]
    fn nothing_is_reported_until_a_program_asks() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        let mut encoder = MouseEncoder::new().unwrap();
        assert!(
            encoder
                .encode(&mut terminal, &GEOMETRY, true, &press(5.0, 5.0))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sgr_reports_press_release_and_motion_once_per_cell() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        let mut encoder = MouseEncoder::new().unwrap();
        // any-event tracking, SGR format
        terminal.vt_write(b"\x1b[?1003h\x1b[?1006h");
        let report = |encoder: &mut MouseEncoder, terminal: &mut Terminal, event: MouseEvent, held: bool| {
            encoder.encode(terminal, &GEOMETRY, held, &event).unwrap()
        };
        assert_eq!(
            report(&mut encoder, &mut terminal, press(25.0, 45.0), true),
            b"\x1b[<0;3;3M"
        );
        let release = MouseEvent {
            action: MouseAction::Release,
            ..press(25.0, 45.0)
        };
        assert_eq!(report(&mut encoder, &mut terminal, release, false), b"\x1b[<0;3;3m");
        let motion = MouseEvent {
            action: MouseAction::Motion,
            button: None,
            ..press(55.0, 45.0)
        };
        assert_eq!(report(&mut encoder, &mut terminal, motion, false), b"\x1b[<35;6;3M");
        let same_cell = MouseEvent { x: 56.0, ..motion };
        assert!(report(&mut encoder, &mut terminal, same_cell, false).is_empty());
        encoder.reset();
        assert_eq!(report(&mut encoder, &mut terminal, same_cell, false), b"\x1b[<35;6;3M");
    }

    #[test]
    fn a_zero_cell_size_is_refused() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        terminal.vt_write(b"\x1b[?1000h");
        let mut encoder = MouseEncoder::new().unwrap();
        let geometry = MouseGeometry {
            cell_width: 0,
            ..GEOMETRY
        };
        assert!(
            encoder
                .encode(&mut terminal, &geometry, true, &press(5.0, 5.0))
                .is_err()
        );
    }
}
