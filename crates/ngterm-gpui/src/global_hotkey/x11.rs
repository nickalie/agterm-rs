//! The global hotkey on X11: a passive grab of the chord's key on the root window, so the server hands its presses to
//! this client whichever window has the keyboard, as `RegisterEventHotKey` hands them to upstream. The grab is made
//! with Caps Lock and Num Lock each on and off (the server matches the exact modifier state), on a connection of the
//! app's own, which a thread reads: a press runs the hotkey; an autorepeat (a release and a press with one timestamp,
//! the server's autorepeat pair) is no new press. Another client holding the same grab refuses it: logged, as upstream
//! leaves an unregistrable chord silent beyond the file's diagnostics.

use std::sync::Arc;

use ngterm_core::keymap::{Chord, Modifiers};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
use x11rb::rust_connection::RustConnection;

/// Caps Lock and Num Lock (Mod2 on every common keymap), either of which may be on while the chord is pressed.
fn locks() -> [ModMask; 4] {
    [
        ModMask::from(0_u16),
        ModMask::LOCK,
        ModMask::M2,
        ModMask::LOCK | ModMask::M2,
    ]
}

/// The X modifiers of `mods`: Control, Alt (Mod1), Super (Mod4) and Shift.
#[must_use]
pub fn modifier_mask(mods: Modifiers) -> ModMask {
    [
        (Modifiers::CONTROL, ModMask::CONTROL),
        (Modifiers::OPTION, ModMask::M1),
        (Modifiers::COMMAND, ModMask::M4),
        (Modifiers::SHIFT, ModMask::SHIFT),
    ]
    .into_iter()
    .filter(|(modifier, _)| mods.contains(*modifier))
    .fold(ModMask::from(0_u16), |mask, (_, bit)| mask | bit)
}

/// The grabs one chord takes: its key with its modifiers, under every lock state.
#[must_use]
pub fn grabs(keycode: u8, mods: Modifiers) -> Vec<(u8, ModMask)> {
    let mask = modifier_mask(mods);
    locks().into_iter().map(|lock| (keycode, mask | lock)).collect()
}

/// The root window's grab of the chord, and the thread reading its presses.
pub struct Grabber {
    conn: Arc<RustConnection>,
    root: u32,
    grabbed: Vec<(u8, ModMask)>,
}

impl Grabber {
    /// A connection to the display GPUI runs on (`DISPLAY`) whose thread calls `fired` on every press of a grabbed
    /// key.
    ///
    /// # Errors
    ///
    /// The X server could not be reached.
    pub fn connect(fired: impl Fn() + Send + Sync + 'static) -> Result<Self, Box<dyn std::error::Error>> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let conn = Arc::new(conn);
        let reader = Arc::clone(&conn);
        std::thread::Builder::new()
            .name("ngterm-global-hotkey".to_owned())
            .spawn(move || read_presses(&reader, &fired))?;
        Ok(Self {
            conn,
            root,
            grabbed: Vec::new(),
        })
    }

    /// Grabs `chord`'s key in place of the one grabbed before; none for `None` or a key with no position.
    pub fn grab(&mut self, chord: Option<&Chord>) {
        for (keycode, mask) in std::mem::take(&mut self.grabbed) {
            if let Err(error) = self.conn.ungrab_key(keycode, self.root, mask) {
                tracing::debug!(%error, "the global hotkey's ungrab failed");
            }
        }
        let Some(chord) = chord else {
            let _ = self.conn.flush();
            return;
        };
        let Some(keycode) = super::keycode(&chord.key).and_then(|keycode| u8::try_from(keycode).ok()) else {
            tracing::warn!(key = chord.key, "the global hotkey's key has no X keycode");
            return;
        };
        for (keycode, mask) in grabs(keycode, chord.mods) {
            let grabbed = self
                .conn
                .grab_key(false, self.root, mask, keycode, GrabMode::ASYNC, GrabMode::ASYNC)
                .map_err(|error| error.to_string())
                .and_then(|cookie| cookie.check().map_err(|error| error.to_string()));
            match grabbed {
                Ok(()) => self.grabbed.push((keycode, mask)),
                Err(error) => {
                    tracing::warn!(%error, "the global hotkey is taken by another application");
                }
            }
        }
        if !self.grabbed.is_empty() {
            tracing::info!(keycode, "global hotkey: grabbed");
        }
        let _ = self.conn.flush();
    }
}

/// Reads the grabbed keys' events until the connection ends, calling `fired` for each new press.
fn read_presses(conn: &RustConnection, fired: &(impl Fn() + Send + Sync)) {
    let mut last_release: Option<(u8, u32)> = None;
    loop {
        match conn.wait_for_event() {
            Ok(Event::KeyPress(press)) => {
                // the server's autorepeat sends a release and a press with one timestamp
                let repeat = last_release == Some((press.detail, press.time));
                last_release = None;
                if !repeat {
                    fired();
                }
            }
            Ok(Event::KeyRelease(release)) => last_release = Some((release.detail, release.time)),
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, "the global hotkey's X connection ended");
                return;
            }
        }
    }
}
