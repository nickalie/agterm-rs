//! keymap.conf's `global-hotkey <chord>` (upstream's `GlobalHotkey`): one chord, taken from every application on the
//! desktop, that toggles the quick terminal, its press from another application never swallowed by the re-show
//! suppression ([`ngterm_core::Core::toggle_quick_terminal_from_global_hotkey`]). Upstream registers it with Carbon's
//! `RegisterEventHotKey` by physical key position, so it survives a layout switch:
//!
//! - X11: by the same position ([`keycode`]: the US key the chord names), a passive grab of the key on the root window
//!   (`XGrabKey`, with Caps Lock and Num Lock either way), on a connection of the app's own whose thread reads the
//!   presses; an autorepeat is no new press ([`x11`]).
//! - Wayland: the desktop's global shortcuts portal (`org.freedesktop.portal.GlobalShortcuts`: KDE Plasma, GNOME 48 and
//!   later, Hyprland), the chord offered as the shortcut's preferred trigger, a key name ([`portal_trigger`]), so how
//!   it fares across a layout switch is the desktop's; the desktop may let the user confirm or change it ([`portal`]). A compositor without the portal (the wlroots ones, niri) gives a
//!   client no global hotkey at all: logged, and its own key bindings run `ngtermctl quick toggle` instead.
//!
//! The chord is registered again whenever the keymap's changes (a reload); none is while the keymap names none
//! (upstream ships no default).

use gpui_kit::App;
use ngterm_core::keymap::{Chord, Modifiers};

use crate::state::{AppState, Poster, post};

#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "linux")]
pub mod x11;

/// The portal shortcut's id and description.
pub const SHORTCUT_ID: &str = "quick-terminal";
pub const SHORTCUT_DESCRIPTION: &str = "Show or hide the quick terminal";

/// The XKB keycode (an X11 keycode) of the key position chord key `key` names: a Latin character by its US position
/// (upstream's `keyCode(forChordKey:)` inverting its `latinKey` table), a named key by its own.
#[must_use]
pub fn keycode(key: &str) -> Option<u32> {
    let named = match key {
        "return" => Some("Enter"),
        "tab" => Some("Tab"),
        "space" => Some("Space"),
        "delete" => Some("Backspace"),
        "left" => Some("ArrowLeft"),
        "right" => Some("ArrowRight"),
        "up" => Some("ArrowUp"),
        "down" => Some("ArrowDown"),
        _ => None,
    };
    if let Some(code) = named {
        return crate::terminal::input::xkb_keycode(code);
    }
    if ngterm_core::keymap::is_function_key(key) {
        return crate::terminal::input::xkb_keycode(&key.to_ascii_uppercase());
    }
    (9..=96).find(|keycode| crate::terminal::input::latin_key(*keycode) == Some(key))
}

/// The keysym name the XDG shortcuts specification spells chord key `key` with (a portal trigger's key).
fn keysym_name(key: &str) -> Option<String> {
    let name = match key {
        "return" => "Return",
        "tab" => "Tab",
        "space" => "space",
        "delete" => "BackSpace",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "`" => "grave",
        "-" => "minus",
        "=" => "equal",
        "[" => "bracketleft",
        "]" => "bracketright",
        "\\" => "backslash",
        ";" => "semicolon",
        "'" => "apostrophe",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        function if ngterm_core::keymap::is_function_key(function) => return Some(function.to_ascii_uppercase()),
        latin if latin.len() == 1 && latin.chars().all(|ch| ch.is_ascii_alphanumeric()) => {
            return Some(latin.to_owned());
        }
        _ => return None,
    };
    Some(name.to_owned())
}

/// `chord` as an XDG shortcuts trigger (`CTRL+ALT+SHIFT+LOGO+<keysym>`), the portal's preferred trigger; `None` for a
/// key with no keysym name.
#[must_use]
pub fn portal_trigger(chord: &Chord) -> Option<String> {
    let mut parts: Vec<String> = [
        (Modifiers::CONTROL, "CTRL"),
        (Modifiers::OPTION, "ALT"),
        (Modifiers::SHIFT, "SHIFT"),
        (Modifiers::COMMAND, "LOGO"),
    ]
    .into_iter()
    .filter(|(modifier, _)| chord.mods.contains(*modifier))
    .map(|(_, name)| name.to_owned())
    .collect();
    parts.push(keysym_name(&chord.key)?);
    Some(parts.join("+"))
}

/// The global hotkey as registered with the desktop.
#[derive(Default)]
pub struct GlobalHotkey {
    /// A sync ran: [`Self::registered`] is what the desktop holds.
    synced: bool,
    /// The chord last registered, `None` for none.
    registered: Option<Chord>,
    #[cfg(target_os = "linux")]
    x11: Option<x11::Grabber>,
    #[cfg(target_os = "linux")]
    portal: Option<portal::Binding>,
    /// The display has no global hotkey of its own (headless, another platform): said once.
    unsupported_logged: bool,
}

impl GlobalHotkey {
    /// Registers the keymap's `chord` when it differs from the one registered, its presses posted to `poster`.
    pub fn sync(&mut self, chord: Option<&Chord>, poster: &Poster, cx: &mut App) {
        if self.synced && self.registered.as_ref() == chord {
            return;
        }
        self.synced = true;
        self.registered = chord.cloned();
        #[cfg(target_os = "linux")]
        match cx.compositor_name() {
            "X11" => return self.grab(chord, poster),
            "Wayland" => return self.bind(chord, poster, cx),
            _ => {}
        }
        if chord.is_some() && !self.unsupported_logged {
            self.unsupported_logged = true;
            tracing::warn!(
                display = cx.compositor_name(),
                "this display has no global hotkey: global-hotkey is ignored"
            );
        }
    }

    #[cfg(target_os = "linux")]
    fn grab(&mut self, chord: Option<&Chord>, poster: &Poster) {
        if self.x11.is_none() {
            if chord.is_none() {
                return;
            }
            match x11::Grabber::connect(fired(poster)) {
                Ok(grabber) => self.x11 = Some(grabber),
                Err(error) => {
                    tracing::warn!(%error, "the global hotkey's X connection did not open");
                    return;
                }
            }
        }
        if let Some(grabber) = &mut self.x11 {
            grabber.grab(chord);
        }
    }

    #[cfg(target_os = "linux")]
    fn bind(&mut self, chord: Option<&Chord>, poster: &Poster, cx: &mut App) {
        let trigger = chord.map(|chord| {
            let trigger = portal_trigger(chord);
            if trigger.is_none() {
                tracing::warn!(key = chord.key, "the global hotkey's key has no portal trigger");
            }
            trigger
        });
        if self.portal.is_none() {
            if chord.is_none() {
                return;
            }
            self.portal = Some(portal::Binding::start(fired(poster), cx));
        }
        if let Some(binding) = &self.portal {
            // a chord without a trigger still binds: the desktop then asks the user for one
            binding.set(trigger.map_or(portal::Wanted::Unbound, portal::Wanted::Bound));
        }
    }
}

/// What a press of the global hotkey runs: the quick terminal's toggle on the main loop.
fn fired(poster: &Poster) -> impl Fn() + Send + Sync + 'static {
    let poster = poster.clone();
    move || post(&poster, AppState::global_hotkey_fired)
}

impl AppState {
    /// The global hotkey was pressed, whichever application had the keyboard (upstream's
    /// `QuickTerminalController.toggle(fromGlobalHotkey: true)`).
    pub fn global_hotkey_fired(&mut self) {
        if self.is_quitting() {
            return;
        }
        tracing::info!("global hotkey: pressed");
        self.core.toggle_quick_terminal_from_global_hotkey();
        self.settle();
    }
}

#[cfg(test)]
mod tests;
