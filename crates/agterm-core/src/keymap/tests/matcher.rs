//! Ported from upstream's `KeybindMatcherTests`.

use std::collections::HashSet;

use uuid::Uuid;

use super::*;

fn ctrl_a() -> Chord {
    chord(CTRL, "a")
}

fn b() -> Chord {
    chord(NONE, "b")
}

fn c() -> Chord {
    chord(NONE, "c")
}

fn cmd_shift_u() -> Chord {
    chord(CMD | SHIFT, "u")
}

fn command() -> KeybindTarget {
    KeybindTarget::Command(Uuid::new_v4())
}

fn matcher(binds: Vec<(Keybind, KeybindTarget)>) -> KeybindMatcher {
    KeybindMatcher::new(binds, HashSet::new())
}

fn repeating(binds: Vec<(Keybind, KeybindTarget)>, targets: &[KeybindTarget]) -> KeybindMatcher {
    KeybindMatcher::new(binds, targets.iter().copied().collect())
}

#[test]
fn simple_chord_fires() {
    let target = command();
    let mut matcher = matcher(vec![(vec![cmd_shift_u()], target)]);
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(target));
    assert!(!matcher.is_armed());
}

#[test]
fn unmatched_single_chord() {
    let mut matcher = matcher(vec![(vec![cmd_shift_u()], command())]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Unmatched);
    assert!(!matcher.is_armed());
}

#[test]
fn sequence_fires_on_second_chord() {
    let target = command();
    let mut matcher = matcher(vec![(vec![ctrl_a(), b()], target)]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert!(matcher.is_armed());
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target));
    assert!(!matcher.is_armed());
}

#[test]
fn wrong_second_chord_resets_and_unmatches() {
    let mut matcher = matcher(vec![(vec![ctrl_a(), b()], command())]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&c()), MatchResult::Unmatched);
    assert!(!matcher.is_armed());
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
}

#[test]
fn reset_clears_pending() {
    let mut matcher = matcher(vec![(vec![ctrl_a(), b()], command())]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    matcher.reset();
    assert!(!matcher.is_armed());
    assert_eq!(matcher.advance(&b()), MatchResult::Unmatched);
}

#[test]
fn two_sequences_sharing_a_leader() {
    let (target_b, target_c) = (command(), command());
    let mut matcher = matcher(vec![(vec![ctrl_a(), b()], target_b), (vec![ctrl_a(), c()], target_c)]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target_b));
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&c()), MatchResult::Fired(target_c));
}

#[test]
fn simple_and_sequence_coexist() {
    let (simple, sequence) = (command(), command());
    let mut matcher = matcher(vec![(vec![cmd_shift_u()], simple), (vec![ctrl_a(), b()], sequence)]);
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(simple));
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(sequence));
}

#[test]
fn re_pressing_the_leader_while_armed_re_arms() {
    let target = command();
    let mut matcher = matcher(vec![(vec![ctrl_a(), b()], target)]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert!(matcher.is_armed());
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target));
}

#[test]
fn a_wrong_chord_while_armed_that_is_itself_a_simple_bind_fires() {
    let (sequence, simple) = (command(), command());
    let mut matcher = matcher(vec![(vec![ctrl_a(), b()], sequence), (vec![cmd_shift_u()], simple)]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(simple));
    assert!(!matcher.is_armed());
}

#[test]
fn alternatives_share_one_target() {
    let target = command();
    let mut matcher = matcher(vec![(vec![cmd_shift_u()], target), (vec![ctrl_a(), b()], target)]);
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(target));
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target));
}

#[test]
fn builtin_and_command_targets_coexist() {
    let command = command();
    let builtin = KeybindTarget::Builtin(BuiltinAction::ToggleSplit);
    let mut matcher = matcher(vec![(vec![cmd_shift_u()], command), (vec![ctrl_a(), b()], builtin)]);
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(command));
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(builtin));
}

#[test]
fn an_empty_matcher_unmatches() {
    let mut matcher = matcher(Vec::new());
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Unmatched);
    assert!(!matcher.is_armed());
}

#[test]
fn a_repeating_sequence_fires_again_on_its_last_chord_alone() {
    let target = KeybindTarget::Builtin(BuiltinAction::NextSession);
    let mut matcher = repeating(vec![(vec![ctrl_a(), b()], target)], &[target]);
    assert_eq!(matcher.advance(&ctrl_a()), MatchResult::Armed);
    assert!(!matcher.is_repeat_tail(&b()), "armed, not repeating");
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target));
    assert!(matcher.is_repeating());
    assert!(!matcher.is_armed());
    assert!(matcher.is_repeat_tail(&b()));
    assert!(!matcher.is_repeat_tail(&ctrl_a()));
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target));
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(target));
}

#[test]
fn the_repeat_window_switches_between_repeating_siblings_only() {
    let next = KeybindTarget::Builtin(BuiltinAction::NextSession);
    let previous = KeybindTarget::Builtin(BuiltinAction::PreviousSession);
    let once = command();
    let d = chord(NONE, "d");
    let mut matcher = repeating(
        vec![
            (vec![ctrl_a(), b()], next),
            (vec![ctrl_a(), c()], previous),
            (vec![ctrl_a(), d.clone()], once),
        ],
        &[next, previous],
    );
    matcher.advance(&ctrl_a());
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(next));
    assert_eq!(matcher.advance(&c()), MatchResult::Fired(previous));
    assert_eq!(matcher.advance(&d), MatchResult::Unmatched);
    assert!(!matcher.is_repeating());
}

#[test]
fn a_key_outside_the_repeat_window_is_matched_afresh() {
    let next = KeybindTarget::Builtin(BuiltinAction::NextSession);
    let simple = command();
    let mut matcher = repeating(
        vec![(vec![ctrl_a(), b()], next), (vec![cmd_shift_u()], simple)],
        &[next],
    );
    matcher.advance(&ctrl_a());
    matcher.advance(&b());
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(simple));
    assert_eq!(matcher.advance(&b()), MatchResult::Unmatched);

    matcher.advance(&ctrl_a());
    matcher.advance(&b());
    assert_eq!(
        matcher.advance(&ctrl_a()),
        MatchResult::Armed,
        "a leader inside the window starts a new sequence"
    );
}

#[test]
fn reset_closes_the_repeat_window() {
    let target = KeybindTarget::Builtin(BuiltinAction::NextSession);
    let mut matcher = repeating(vec![(vec![ctrl_a(), b()], target)], &[target]);
    matcher.advance(&ctrl_a());
    matcher.advance(&b());
    matcher.reset();
    assert_eq!(matcher.advance(&b()), MatchResult::Unmatched);
}

#[test]
fn a_single_chord_and_a_non_repeating_sequence_open_no_window() {
    let simple = KeybindTarget::Builtin(BuiltinAction::ToggleSplit);
    let sequence = KeybindTarget::Builtin(BuiltinAction::NextSession);
    let mut matcher = repeating(
        vec![(vec![cmd_shift_u()], simple), (vec![ctrl_a(), b()], sequence)],
        &[simple],
    );
    assert_eq!(matcher.advance(&cmd_shift_u()), MatchResult::Fired(simple));
    assert!(!matcher.is_repeating());
    matcher.advance(&ctrl_a());
    assert_eq!(matcher.advance(&b()), MatchResult::Fired(sequence));
    assert!(!matcher.is_repeating());
}
