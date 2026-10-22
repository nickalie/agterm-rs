//! Whether a terminal program may read or write the clipboard without asking (upstream's
//! `ClipboardPromptPolicy.swift`).

/// The direction of a clipboard access a terminal program requests (OSC 52 or the Kitty clipboard protocol): reading
/// the system clipboard into the terminal stream (exfiltration risk) or writing the terminal's data to it (poisoning
/// risk).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardAccess {
    Read,
    Write,
}

/// What to do with a clipboard access request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardDecision {
    /// Perform it without asking (the user allowed it for the session).
    Allow,
    /// Refuse it silently (the user denied it for the session).
    Deny,
    /// Ask the user (no remembered choice yet).
    Prompt,
}

/// The user's "don't ask again this session" choice per direction, prompting while none is set. Read and write are
/// independent, so allowing writes never silently allows reads. The app owns one for its whole run, prompts on
/// [`ClipboardDecision::Prompt`] and records the choice with [`ClipboardPromptPolicy::remember`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClipboardPromptPolicy {
    read: Option<bool>,
    write: Option<bool>,
}

impl ClipboardPromptPolicy {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The remembered choice for `access`, or [`ClipboardDecision::Prompt`] when none is set.
    #[must_use]
    pub fn decision(&self, access: ClipboardAccess) -> ClipboardDecision {
        match self.choice(access) {
            Some(true) => ClipboardDecision::Allow,
            Some(false) => ClipboardDecision::Deny,
            None => ClipboardDecision::Prompt,
        }
    }

    /// Records the user's "don't ask again this session" choice for `access`; a later one overrides it.
    pub fn remember(&mut self, access: ClipboardAccess, allow: bool) {
        match access {
            ClipboardAccess::Read => self.read = Some(allow),
            ClipboardAccess::Write => self.write = Some(allow),
        }
    }

    fn choice(self, access: ClipboardAccess) -> Option<bool> {
        match access {
            ClipboardAccess::Read => self.read,
            ClipboardAccess::Write => self.write,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardAccess::{Read, Write};
    use super::*;

    #[test]
    fn defaults_to_prompt_for_both_directions() {
        let policy = ClipboardPromptPolicy::new();
        assert_eq!(policy.decision(Read), ClipboardDecision::Prompt);
        assert_eq!(policy.decision(Write), ClipboardDecision::Prompt);
    }

    #[test]
    fn remember_allow_makes_the_direction_allow() {
        let mut policy = ClipboardPromptPolicy::new();
        policy.remember(Read, true);
        assert_eq!(policy.decision(Read), ClipboardDecision::Allow);
    }

    #[test]
    fn remember_deny_makes_the_direction_deny() {
        let mut policy = ClipboardPromptPolicy::new();
        policy.remember(Write, false);
        assert_eq!(policy.decision(Write), ClipboardDecision::Deny);
    }

    #[test]
    fn directions_are_independent() {
        let mut policy = ClipboardPromptPolicy::new();
        policy.remember(Read, true);
        assert_eq!(policy.decision(Read), ClipboardDecision::Allow);
        assert_eq!(policy.decision(Write), ClipboardDecision::Prompt);
    }

    #[test]
    fn remembering_write_leaves_read_prompting() {
        let mut policy = ClipboardPromptPolicy::new();
        policy.remember(Write, false);
        assert_eq!(policy.decision(Write), ClipboardDecision::Deny);
        assert_eq!(policy.decision(Read), ClipboardDecision::Prompt);
    }

    #[test]
    fn a_later_remember_overrides_an_earlier_one() {
        let mut policy = ClipboardPromptPolicy::new();
        policy.remember(Read, true);
        policy.remember(Read, false);
        assert_eq!(policy.decision(Read), ClipboardDecision::Deny);
    }
}
