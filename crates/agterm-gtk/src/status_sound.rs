//! Status sounds (upstream's `StatusSoundPlayer.swift`). Linux has none of the named macOS system sounds upstream plays,
//! so every name, `default` and `beep` included, sounds the display's alert bell, as the replaced fork does; the names
//! are still validated against upstream's list by the core. A rapid repeat of one name is throttled as upstream's
//! ([`SoundThrottle`]), so a burst of `session.status --sound` calls cannot machine-gun the bell.

use std::cell::RefCell;
use std::time::Instant;

use agterm_core::sound_throttle::SoundThrottle;
use gtk::prelude::DisplayExt;

thread_local! {
    static THROTTLE: RefCell<SoundThrottle> = RefCell::new(SoundThrottle::default());
}

/// Plays status sound `name` unless the same name played within the throttle window.
pub fn play(name: &str) {
    if !THROTTLE.with(|throttle| throttle.borrow_mut().allow(name, Instant::now())) {
        tracing::debug!(name, "status sound throttled");
        return;
    }
    if let Some(display) = gtk::gdk::Display::default() {
        display.beep();
    } else {
        tracing::debug!(name, "no display to sound the status alert on");
    }
}
