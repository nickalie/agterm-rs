//! Status sounds (upstream's `StatusSoundPlayer.swift`). Linux has none of the named macOS system sounds upstream plays,
//! so every name, `default` and `beep` included, sounds the display's alert bell, as the replaced fork does; the names
//! are still validated against upstream's list by the core. Upstream throttles each clip on its own name
//! ([`SoundThrottle`]); here every name rings the same bell, so the throttle keys on the bell itself, and a burst of
//! `session.status --sound` calls, whatever names they give, rings it at most once per window.

use std::cell::RefCell;
use std::time::Instant;

use agterm_core::sound_throttle::SoundThrottle;
use gtk::prelude::DisplayExt;

/// The one throttle key: every status sound is the same bell.
const BELL: &str = "bell";

thread_local! {
    static THROTTLE: RefCell<SoundThrottle> = RefCell::new(SoundThrottle::default());
}

/// Whether the bell may ring for a sound at `now`: any status sound within the throttle window of the last ring is
/// suppressed, whatever its name.
fn allow(throttle: &mut SoundThrottle, now: Instant) -> bool {
    throttle.allow(BELL, now)
}

/// Plays status sound `name` unless the bell rang within the throttle window.
pub fn play(name: &str) {
    if !THROTTLE.with(|throttle| allow(&mut throttle.borrow_mut(), Instant::now())) {
        tracing::debug!(name, "status sound throttled");
        return;
    }
    if let Some(display) = gtk::gdk::Display::default() {
        display.beep();
    } else {
        tracing::debug!(name, "no display to sound the status alert on");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn different_names_share_the_one_bell_throttle() {
        let mut throttle = SoundThrottle::default();
        let t0 = Instant::now();
        assert!(allow(&mut throttle, t0), "Ping rings");
        assert!(
            !allow(&mut throttle, t0 + Duration::from_millis(10)),
            "Hero right after does not"
        );
        assert!(allow(&mut throttle, t0 + Duration::from_millis(200)));
    }
}
