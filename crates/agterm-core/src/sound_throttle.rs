//! Rapid repeats of one status sound, suppressed (upstream's `SoundThrottle.swift`): a burst of
//! `session.status --sound` calls, or repeated `blocked` transitions, cannot machine-gun one clip. The last allowed
//! play is tracked per sound name, so a different sound is never suppressed. The clock is the caller's, so the window
//! is testable without real time.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How close a repeat of the same sound may follow it (upstream's `StatusSoundPlayer` throttle).
pub const STATUS_SOUND_WINDOW: Duration = Duration::from_millis(200);

/// The last allowed play of each sound name.
#[derive(Debug, Clone)]
pub struct SoundThrottle {
    window: Duration,
    last_played: HashMap<String, Instant>,
}

impl SoundThrottle {
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last_played: HashMap::new(),
        }
    }

    /// Whether `name` may play at `now`, recording the play when it may. A replay still inside the window is refused
    /// without changing anything, so the window runs from the last allowed play, not the last attempt.
    pub fn allow(&mut self, name: &str, now: Instant) -> bool {
        if let Some(last) = self.last_played.get(name)
            && now.saturating_duration_since(*last) < self.window
        {
            return false;
        }
        self.last_played.insert(name.to_owned(), now);
        true
    }
}

impl Default for SoundThrottle {
    fn default() -> Self {
        Self::new(STATUS_SOUND_WINDOW)
    }
}

/// Ported from upstream's `SoundThrottleTests`.
#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn the_first_play_of_a_name_is_allowed() {
        assert!(SoundThrottle::default().allow("Ping", Instant::now()));
    }

    #[test]
    fn the_same_sound_is_suppressed_within_the_window() {
        let mut throttle = SoundThrottle::new(ms(200));
        let t0 = Instant::now();
        assert!(throttle.allow("Ping", t0));
        assert!(!throttle.allow("Ping", t0 + ms(1)));
        assert!(!throttle.allow("Ping", t0 + ms(199)));
    }

    #[test]
    fn the_same_sound_plays_at_or_past_the_window_boundary() {
        let mut throttle = SoundThrottle::new(ms(200));
        let t0 = Instant::now();
        assert!(throttle.allow("Ping", t0));
        assert!(throttle.allow("Ping", t0 + ms(200)));
        assert!(throttle.allow("Ping", t0 + ms(450)));
    }

    #[test]
    fn different_sounds_throttle_independently() {
        let mut throttle = SoundThrottle::new(ms(200));
        let t0 = Instant::now();
        assert!(throttle.allow("Ping", t0));
        assert!(throttle.allow("Hero", t0 + ms(10)));
        assert!(!throttle.allow("Ping", t0 + ms(20)));
        assert!(!throttle.allow("Hero", t0 + ms(30)));
    }

    #[test]
    fn a_suppressed_replay_does_not_advance_the_window() {
        let mut throttle = SoundThrottle::new(ms(200));
        let t0 = Instant::now();
        assert!(throttle.allow("Ping", t0));
        assert!(!throttle.allow("Ping", t0 + ms(150)));
        assert!(throttle.allow("Ping", t0 + ms(200)));
    }
}
