//! The leader/sequence state machine (upstream's `KeybindMatcher.swift`): turns a stream of chords into fires.
//! Deadline-free: the leader timeout and the repeat window's release timer are the frontend's, which call
//! [`KeybindMatcher::reset`].

use std::collections::HashSet;

use super::keybind::{Chord, Keybind, KeybindTarget, is_strict_keybind_prefix};

/// The outcome of feeding one chord to a [`KeybindMatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchResult {
    /// The pending prefix plus this chord is a bound keybind; the matcher reset, keeping only a repeatable
    /// sequence's repeat window.
    Fired(KeybindTarget),
    /// The pending prefix plus this chord starts a longer bind; the leader is armed.
    Armed,
    /// Nothing starts with the pending prefix plus this chord; the matcher reset.
    Unmatched,
}

/// Matches chords against `(Keybind, KeybindTarget)` pairs, holding the chords typed so far.
#[derive(Debug, Clone)]
pub struct KeybindMatcher {
    binds: Vec<(Keybind, KeybindTarget)>,
    repeating: HashSet<KeybindTarget>,
    pending: Vec<Chord>,
    /// The prefix a repeatable sequence just fired under (tmux `bind -r`): until [`Self::reset`], the last chord
    /// of any repeatable bind sharing it fires again without retyping the prefix.
    repeat_prefix: Option<Vec<Chord>>,
}

impl KeybindMatcher {
    /// `repeating` names the targets whose leader sequences stay live for another press after firing.
    #[must_use]
    pub fn new(binds: Vec<(Keybind, KeybindTarget)>, repeating: HashSet<KeybindTarget>) -> Self {
        Self {
            binds,
            repeating,
            pending: Vec::new(),
            repeat_prefix: None,
        }
    }

    /// Whether a sequence is partway through (a leader is armed).
    #[must_use]
    pub fn is_armed(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether a repeatable sequence just fired and its prefix is still live. Not [`Self::is_armed`]: a key that
    /// repeats nothing ends the window and is matched afresh.
    #[must_use]
    pub fn is_repeating(&self) -> bool {
        self.repeat_prefix.is_some()
    }

    /// Feeds one chord: an exact match fires and resets, a strict prefix arms, anything else resets unmatched.
    /// While armed, a chord completing nothing restarts from itself, so re-pressing a leader re-arms it and a
    /// simple bind still fires.
    pub fn advance(&mut self, chord: &Chord) -> MatchResult {
        if let Some(prefix) = self.repeat_prefix.take()
            && let Some(target) = self.repeat_target(&prefix, chord)
        {
            self.repeat_prefix = Some(prefix);
            return MatchResult::Fired(target);
        }

        let mut candidate = self.pending.clone();
        candidate.push(chord.clone());

        if let Some(target) = self
            .binds
            .iter()
            .find(|(keybind, _)| *keybind == candidate)
            .map(|(_, target)| *target)
        {
            self.pending.clear();
            if candidate.len() > 1 && self.repeating.contains(&target) {
                candidate.pop();
                self.repeat_prefix = Some(candidate);
            }
            return MatchResult::Fired(target);
        }

        if self
            .binds
            .iter()
            .any(|(keybind, _)| is_strict_keybind_prefix(&candidate, keybind))
        {
            self.pending = candidate;
            return MatchResult::Armed;
        }

        if self.is_armed() {
            self.pending.clear();
            return self.advance(chord);
        }
        MatchResult::Unmatched
    }

    /// Clears the pending prefix and any repeat window (Esc, or the frontend's timeout).
    pub fn reset(&mut self) {
        self.pending.clear();
        self.repeat_prefix = None;
    }

    /// Whether `chord` would fire again inside the open repeat window, so the frontend can route a held tail's
    /// autorepeat to [`Self::advance`] while every other consumed autorepeat stays swallowed.
    #[must_use]
    pub fn is_repeat_tail(&self, chord: &Chord) -> bool {
        self.repeat_prefix
            .as_ref()
            .is_some_and(|prefix| self.repeat_target(prefix, chord).is_some())
    }

    fn repeat_target(&self, prefix: &[Chord], tail: &Chord) -> Option<KeybindTarget> {
        self.binds
            .iter()
            .find(|(keybind, target)| {
                self.repeating.contains(target)
                    && keybind.len() == prefix.len() + 1
                    && keybind.starts_with(prefix)
                    && keybind.last() == Some(tail)
            })
            .map(|(_, target)| *target)
    }
}
