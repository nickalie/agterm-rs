//! Input into a pane: keys, the mouse, focus reports, pastes and viewport scrolling.
//!
//! Each is encoded under the VT lock, because the encoders read the terminal's current modes (cursor and keypad
//! application, the Kitty keyboard flags, the mouse tracking mode and format, bracketed paste), and the bytes are
//! queued for the child while the lock is still held, so they can never overtake or fall behind a reply the terminal
//! queued in between. The queue is the pane's bounded input queue ([`Pane::write`]): a child that reads nothing makes
//! input fail with [`PaneError::Backpressure`] instead of growing it.
//!
//! The key is the toolkit-neutral [`KeyInput`]: the frontend builds one per key event, routes it through agterm's
//! keymap and ghostty's binds first ([`KeyInput::binds_event`], `agterm_core::ghostty_config::binds::Keyboard`), and
//! hands it to [`Pane::key`] when the binds let it through.

use agterm_core::ghostty_config::binds::{KeyAction, KeyEvent, Mods, PHYSICAL_KEYS, PhysicalKey};
use agterm_ghostty_vt as vt;
use bitflags::bitflags;

use crate::PaneError;
use crate::effects::ClipboardLocation;
use crate::pane::{Pane, Vt, lock};
use crate::pty::GridSize;

bitflags! {
    /// Which held modifiers are the right-hand key (meaningful only with the modifier itself held).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct ModSides: u8 {
        const SHIFT = 1;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
    }
}

/// A key event as the frontend saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput<'a> {
    pub action: KeyAction,
    /// The key's position on the keyboard, whatever the layout; [`PhysicalKey::UNIDENTIFIED`] when unknown.
    pub key: PhysicalKey,
    pub mods: Mods,
    pub sides: ModSides,
    /// The modifiers the layout used up producing `text` (shift for `A`).
    pub consumed: Mods,
    /// Part of an input-method composition (a dead key, a preedit).
    pub composing: bool,
    /// What the key types under the layout, before Ctrl/Alt transformations; empty for none.
    pub text: &'a str,
    /// The character the key types with no modifier, under the layout (the Kitty protocol's key code).
    pub unshifted: Option<char>,
}

impl<'a> KeyInput<'a> {
    /// A press of `key` with `mods`, typing `text`; the unshifted character is guessed as the lower case of a
    /// one-character `text` (right for letters; a frontend that knows the layout sets [`KeyInput::unshifted`]
    /// itself, e.g. `1` for a shifted `!`).
    #[must_use]
    pub fn press(key: PhysicalKey, mods: Mods, text: &'a str) -> Self {
        Self {
            action: KeyAction::Press,
            key,
            mods,
            sides: ModSides::empty(),
            consumed: Mods::empty(),
            composing: false,
            text,
            unshifted: text
                .chars()
                .next()
                .filter(|_| text.chars().count() == 1)
                .map(|c| c.to_lowercase().next().unwrap_or(c)),
        }
    }

    /// The event ghostty's binds look at (`binds::Keyboard::handle`).
    #[must_use]
    pub fn binds_event(&self) -> KeyEvent<'a> {
        KeyEvent {
            action: self.action,
            key: self.key,
            mods: self.mods,
            text: self.text,
            unshifted: self.unshifted,
        }
    }

    fn engine(&self) -> vt::KeyEvent<'a> {
        vt::KeyEvent {
            action: match self.action {
                KeyAction::Press => vt::KeyAction::Press,
                KeyAction::Repeat => vt::KeyAction::Repeat,
                KeyAction::Release => vt::KeyAction::Release,
            },
            key: engine_key(self.key),
            mods: engine_mods(self.mods, self.sides),
            consumed_mods: engine_mods(self.consumed, ModSides::empty()),
            composing: self.composing,
            text: self.text,
            unshifted: self.unshifted,
        }
    }
}

/// The engine's key for `key`: ghostty's key names and the C API's key enum list the keys in one order.
fn engine_key(key: PhysicalKey) -> vt::Key {
    PHYSICAL_KEYS
        .iter()
        .position(|name| *name == key.name())
        .and_then(vt::Key::from_index)
        .unwrap_or(vt::Key::UNIDENTIFIED)
}

fn engine_mods(mods: Mods, sides: ModSides) -> vt::KeyMods {
    let mut engine = vt::KeyMods::empty();
    for (mod_, side, held, right) in [
        (
            Mods::SHIFT,
            ModSides::SHIFT,
            vt::KeyMods::SHIFT,
            vt::KeyMods::SHIFT_RIGHT,
        ),
        (Mods::CTRL, ModSides::CTRL, vt::KeyMods::CTRL, vt::KeyMods::CTRL_RIGHT),
        (Mods::ALT, ModSides::ALT, vt::KeyMods::ALT, vt::KeyMods::ALT_RIGHT),
        (
            Mods::SUPER,
            ModSides::SUPER,
            vt::KeyMods::SUPER,
            vt::KeyMods::SUPER_RIGHT,
        ),
    ] {
        if mods.contains(mod_) {
            engine |= held;
            if sides.contains(side) {
                engine |= right;
            }
        }
    }
    engine.set(vt::KeyMods::CAPS_LOCK, mods.contains(Mods::CAPS_LOCK));
    engine.set(vt::KeyMods::NUM_LOCK, mods.contains(Mods::NUM_LOCK));
    engine
}

/// A pointer event's kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MouseAction {
    #[default]
    Press,
    Release,
    Motion,
}

/// A mouse button; a wheel step up is [`MouseButton::Four`], down [`MouseButton::Five`], left/right six/seven.
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
    const ALL: [Self; 11] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Four,
        Self::Five,
        Self::Six,
        Self::Seven,
        Self::Eight,
        Self::Nine,
        Self::Ten,
        Self::Eleven,
    ];

    fn engine(self) -> vt::MouseButton {
        match self {
            Self::Left => vt::MouseButton::Left,
            Self::Right => vt::MouseButton::Right,
            Self::Middle => vt::MouseButton::Middle,
            Self::Four => vt::MouseButton::Four,
            Self::Five => vt::MouseButton::Five,
            Self::Six => vt::MouseButton::Six,
            Self::Seven => vt::MouseButton::Seven,
            Self::Eight => vt::MouseButton::Eight,
            Self::Nine => vt::MouseButton::Nine,
            Self::Ten => vt::MouseButton::Ten,
            Self::Eleven => vt::MouseButton::Eleven,
        }
    }

    /// The button's bit in the held set; none for a wheel step (buttons four to seven), which is never released, as
    /// Ghostty keeps wheel reports out of its click state.
    fn bit(self) -> u16 {
        if matches!(self, Self::Four | Self::Five | Self::Six | Self::Seven) {
            return 0;
        }
        let index = Self::ALL.iter().position(|button| *button == self).unwrap_or(0);
        1 << index
    }
}

/// A pointer event; the position is in surface pixels from the pane's top left, padding included.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MouseInput {
    pub action: MouseAction,
    /// `None` for a motion with no button.
    pub button: Option<MouseButton>,
    pub mods: Mods,
    pub x: f32,
    pub y: f32,
}

/// The space around the grid inside the pane's surface, in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Padding {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

/// Why a paste happens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PasteSource {
    /// The user pasted from a clipboard (a key binding, a menu, a middle click): a program that asked for Kitty
    /// paste events gets one.
    #[default]
    Clipboard,
    /// Text inserted another way (an IME commit, a drop, scripted input): always written as text.
    Text,
}

/// What a paste did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Paste {
    /// The text (framed for the terminal's modes) or a paste event went to the child.
    Written,
    /// Nothing to paste.
    Empty,
    /// The text could run a command (a line ending outside bracketed paste, the bracketed-paste terminator) and the
    /// paste did not allow it: nothing was written. Confirm with the user and paste again with `allow_unsafe`.
    Unsafe,
}

/// A viewport scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scroll {
    Top,
    Bottom,
    /// By rows; negative is up.
    Rows(isize),
    /// By shell prompts (semantic prompt marks); negative is back.
    Prompts(isize),
}

/// The input side of the VT lock: the encoders and what the mouse encoder needs to know.
pub(crate) struct InputState {
    keys: vt::KeyEncoder,
    mouse: vt::MouseEncoder,
    /// The buttons held, one bit per [`MouseButton`] (wheel steps never count).
    pressed: u16,
    padding: Padding,
    /// The config's `mouse-reporting`: whether a program may have the mouse at all.
    reporting: bool,
}

impl InputState {
    pub(crate) fn new() -> Result<Self, vt::Error> {
        Ok(Self {
            keys: vt::KeyEncoder::new()?,
            mouse: vt::MouseEncoder::new()?,
            pressed: 0,
            padding: Padding::default(),
            reporting: true,
        })
    }

    /// Whether pointer events go to the program: it turned a tracking mode on and the config lets it
    /// (Ghostty's `isMouseReporting`).
    pub(crate) fn reporting(&self, terminal: &vt::Terminal) -> bool {
        self.reporting && terminal.mouse_tracking()
    }

    /// Encode one pointer event for the program (the held-button set already updated).
    pub(crate) fn encode_mouse(
        &mut self,
        terminal: &mut vt::Terminal,
        size: GridSize,
        input: &MouseInput,
    ) -> Result<Vec<u8>, vt::Error> {
        let event = vt::MouseEvent {
            action: match input.action {
                MouseAction::Press => vt::MouseAction::Press,
                MouseAction::Release => vt::MouseAction::Release,
                MouseAction::Motion => vt::MouseAction::Motion,
            },
            button: input.button.map(MouseButton::engine),
            mods: engine_mods(input.mods, ModSides::empty()),
            x: input.x,
            y: input.y,
        };
        let geometry = self.geometry(size);
        let pressed = self.pressed != 0;
        self.mouse.encode(terminal, &geometry, pressed, &event)
    }

    fn geometry(&self, size: GridSize) -> vt::MouseGeometry {
        let padding = self.padding;
        vt::MouseGeometry {
            screen_width: u32::from(size.cols) * size.cell_width + padding.left + padding.right,
            screen_height: u32::from(size.rows) * size.cell_height + padding.top + padding.bottom,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
            padding_top: padding.top,
            padding_bottom: padding.bottom,
            padding_right: padding.right,
            padding_left: padding.left,
        }
    }
}

impl Pane {
    /// Send a key to the program, encoded for the terminal's current keyboard modes. Whether it sent anything (a
    /// lone modifier or a release outside the Kitty protocol's event reporting sends nothing).
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when encoding fails; [`PaneError::Backpressure`] or [`PaneError::Closed`] from the
    /// input queue.
    pub fn key(&self, input: &KeyInput<'_>) -> Result<bool, PaneError> {
        let mut vt = lock(&self.shared().vt);
        let Vt {
            terminal, input: state, ..
        } = &mut *vt;
        let bytes = state
            .keys
            .encode(terminal, &input.engine())
            .map_err(PaneError::terminal)?;
        self.queue(&bytes)
    }

    /// Report a pointer event to the program, as the mouse mode it turned on asks; whether anything was reported
    /// (nothing while no program asked or `mouse-reporting` is off, see [`Pane::mouse_tracking`]). A wheel goes
    /// through [`Pane::wheel`] instead.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when encoding fails (a zero cell size); [`PaneError::Backpressure`] or
    /// [`PaneError::Closed`] from the input queue.
    pub fn mouse(&self, input: &MouseInput) -> Result<bool, PaneError> {
        let mut vt = lock(&self.shared().vt);
        let size = vt.size;
        let Vt {
            terminal, input: state, ..
        } = &mut *vt;
        if let Some(button) = input.button {
            match input.action {
                MouseAction::Press => state.pressed |= button.bit(),
                MouseAction::Release => state.pressed &= !button.bit(),
                MouseAction::Motion => {}
            }
        }
        if !state.reporting(terminal) {
            return Ok(false);
        }
        let bytes = state.encode_mouse(terminal, size, input).map_err(PaneError::terminal)?;
        self.queue(&bytes)
    }

    /// Whether pointer events go to the program (it turned mouse reporting on and `mouse-reporting` allows it); the
    /// frontend selects text with the mouse only while they do not.
    #[must_use]
    pub fn mouse_tracking(&self) -> bool {
        let vt = lock(&self.shared().vt);
        vt.input.reporting(&vt.terminal)
    }

    /// The config's `mouse-reporting` (on by default): off, programs never get the mouse.
    pub fn set_mouse_reporting(&self, enabled: bool) {
        lock(&self.shared().vt).input.reporting = enabled;
    }

    /// The padding around the grid inside the surface the mouse positions are measured in.
    pub fn set_padding(&self, padding: Padding) {
        lock(&self.shared().vt).input.padding = padding;
    }

    /// The pane gained or lost keyboard focus: a program in focus-reporting mode (1004) gets `CSI I` / `CSI O`.
    /// Whether a report was sent.
    ///
    /// # Errors
    ///
    /// [`PaneError::Backpressure`] or [`PaneError::Closed`] from the input queue.
    pub fn focus(&self, focused: bool) -> Result<bool, PaneError> {
        let vt = lock(&self.shared().vt);
        if !vt.terminal.mode(vt::Mode::dec(1004)) {
            return Ok(false);
        }
        self.queue(&vt::encode_focus(focused))
    }

    /// Paste `text` per the terminal's state: bracketed (mode 2004) or with its line endings as carriage returns,
    /// unsafe control bytes replaced; a Kitty paste event instead when the program asked for one (mode 5522) and the
    /// paste came from a clipboard. Text that could run a command is refused unless `allow_unsafe`.
    ///
    /// # Errors
    ///
    /// [`PaneError::Terminal`] when the library fails the paste (nothing is written then);
    /// [`PaneError::Backpressure`] or [`PaneError::Closed`] from the input queue.
    pub fn paste(
        &self,
        text: &str,
        source: PasteSource,
        location: ClipboardLocation,
        allow_unsafe: bool,
    ) -> Result<Paste, PaneError> {
        let mut vt = lock(&self.shared().vt);
        // the terminal writes the paste through its PTY callback; divert it into one piece of input, so a paste is
        // queued whole or refused whole
        *lock(&self.shared().divert) = Some(Vec::new());
        let source = match source {
            PasteSource::Clipboard => vt::PasteSource::Clipboard,
            PasteSource::Text => vt::PasteSource::Text,
        };
        let outcome = vt.terminal.paste(text, source, location.engine(), allow_unsafe);
        let bytes = lock(&self.shared().divert).take().unwrap_or_default();
        let paste = match outcome.map_err(PaneError::terminal)? {
            vt::PasteOutcome::Written => Paste::Written,
            vt::PasteOutcome::Empty => return Ok(Paste::Empty),
            vt::PasteOutcome::Unsafe => return Ok(Paste::Unsafe),
        };
        self.queue(&bytes)?;
        drop(vt);
        Ok(paste)
    }

    /// Scroll the viewport.
    pub fn scroll(&self, scroll: Scroll) {
        let engine = match scroll {
            Scroll::Top => vt::Scroll::Top,
            Scroll::Bottom => vt::Scroll::Bottom,
            Scroll::Rows(rows) => vt::Scroll::Rows(rows),
            Scroll::Prompts(prompts) => vt::Scroll::Prompts(prompts),
        };
        lock(&self.shared().vt).terminal.scroll_viewport(engine);
        self.shared().wake();
    }

    /// Queue encoded input (the caller holds the VT lock); whether there was any.
    pub(crate) fn queue(&self, bytes: &[u8]) -> Result<bool, PaneError> {
        if bytes.is_empty() {
            return Ok(false);
        }
        self.write(bytes)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> PhysicalKey {
        PhysicalKey::new(name).unwrap()
    }

    #[test]
    fn every_key_name_maps_to_the_engine_key_of_the_same_position() {
        assert_eq!(PHYSICAL_KEYS.len(), vt::Key::COUNT);
        for (name, engine) in [
            ("unidentified", vt::Key::UNIDENTIFIED),
            ("key_a", vt::Key::A),
            ("key_c", vt::Key::C),
            ("enter", vt::Key::ENTER),
            ("arrow_up", vt::Key::ARROW_UP),
            ("f1", vt::Key::F1),
            ("f12", vt::Key::F12),
            ("shift_left", vt::Key::SHIFT_LEFT),
        ] {
            assert_eq!(engine_key(key(name)), engine, "{name}");
        }
    }

    #[test]
    fn sides_count_only_with_their_modifier() {
        assert_eq!(
            engine_mods(Mods::SHIFT | Mods::CAPS_LOCK, ModSides::SHIFT | ModSides::CTRL),
            vt::KeyMods::SHIFT | vt::KeyMods::SHIFT_RIGHT | vt::KeyMods::CAPS_LOCK
        );
        assert_eq!(engine_mods(Mods::empty(), ModSides::all()), vt::KeyMods::empty());
    }

    #[test]
    fn a_press_derives_its_unshifted_character() {
        let press = KeyInput::press(key("key_a"), Mods::SHIFT, "A");
        assert_eq!(press.unshifted, Some('a'));
        assert_eq!(KeyInput::press(key("enter"), Mods::empty(), "").unshifted, None);
        let event = press.binds_event();
        assert_eq!((event.key, event.text, event.unshifted), (key("key_a"), "A", Some('a')));
    }

    #[test]
    fn buttons_have_distinct_bits_and_wheel_steps_none() {
        let wheel = [
            MouseButton::Four,
            MouseButton::Five,
            MouseButton::Six,
            MouseButton::Seven,
        ];
        let bits: Vec<u16> = MouseButton::ALL
            .iter()
            .filter(|button| !wheel.contains(button))
            .map(|button| button.bit())
            .collect();
        for (index, bit) in bits.iter().enumerate() {
            assert_eq!(bit.count_ones(), 1);
            assert!(!bits[..index].contains(bit));
        }
        assert!(wheel.iter().all(|button| button.bit() == 0));
    }
}
