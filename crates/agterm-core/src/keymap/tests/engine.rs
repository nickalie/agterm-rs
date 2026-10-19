//! Ported from upstream's `CustomCommandEngineTests`.

use std::collections::BTreeSet;

use super::*;

fn ctrl_a() -> Chord {
    chord(CTRL, "a")
}

fn g() -> Chord {
    chord(NONE, "g")
}

fn x() -> Chord {
    chord(NONE, "x")
}

fn cmd_shift_e() -> Chord {
    chord(CMD | SHIFT, "e")
}

fn engine(commands: &[CustomCommand]) -> CustomCommandEngine {
    CustomCommandEngine::new(commands, &BTreeMap::new(), &BTreeSet::new())
}

#[test]
fn fires_a_simple_command() {
    let command = CustomCommand::new("edit", "zed {AGT_SESSION_PWD}", "cmd+shift+e");
    let mut engine = engine(std::slice::from_ref(&command));
    assert_eq!(engine.advance(&cmd_shift_e()), EngineOutcome::Fired(command));
    assert!(!engine.is_armed());
}

#[test]
fn arms_then_fires_a_leader_command() {
    let command = CustomCommand::new("git", "git status", "ctrl+a>g");
    let mut engine = engine(std::slice::from_ref(&command));
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert!(engine.is_armed());
    assert_eq!(engine.advance(&g()), EngineOutcome::Fired(command));
    assert!(!engine.is_armed());
}

#[test]
fn an_unmatched_chord_passes_through() {
    let mut engine = engine(&[CustomCommand::new("edit", "zed", "cmd+shift+e")]);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Unmatched);
    assert!(!engine.is_armed());
}

#[test]
fn reset_abandons_a_leader_sequence() {
    let mut engine = engine(&[CustomCommand::new("git", "git status", "ctrl+a>g")]);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    engine.reset();
    assert!(!engine.is_armed());
    assert_eq!(engine.advance(&g()), EngineOutcome::Unmatched);
}

#[test]
fn a_palette_only_command_never_matches_a_chord() {
    let mut engine = engine(&[CustomCommand::new("palette", "touch /tmp/x", "")]);
    assert_eq!(engine.advance(&cmd_shift_e()), EngineOutcome::Unmatched);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Unmatched);
}

#[test]
fn an_invalid_shortcut_never_matches_a_chord() {
    let mut engine = engine(&[CustomCommand::new("bad", "echo bad", "ctrl+missing")]);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Unmatched);
}

#[test]
fn a_wrong_leader_continuation_can_restart_as_a_fresh_leader() {
    let command = CustomCommand::new("git", "git status", "ctrl+a>g");
    let mut engine = engine(std::slice::from_ref(&command));
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(engine.advance(&g()), EngineOutcome::Fired(command));
}

#[test]
fn a_wrong_leader_continuation_can_fire_a_simple_command() {
    let sequence = CustomCommand::new("git", "git status", "ctrl+a>g");
    let simple = CustomCommand::new("edit", "zed", "cmd+shift+e");
    let mut engine = engine(&[sequence, simple.clone()]);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(engine.advance(&cmd_shift_e()), EngineOutcome::Fired(simple));
    assert!(!engine.is_armed());
}

#[test]
fn an_empty_engine_unmatches() {
    let mut engine = engine(&[]);
    assert_eq!(engine.advance(&x()), EngineOutcome::Unmatched);
    assert!(!engine.is_armed());
}

#[test]
fn either_alternative_fires_the_same_command() {
    let command = CustomCommand::new("mc", "mc", "cmd+shift+e|ctrl+a>g");
    let mut engine = engine(std::slice::from_ref(&command));
    assert_eq!(engine.advance(&cmd_shift_e()), EngineOutcome::Fired(command.clone()));
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(engine.advance(&g()), EngineOutcome::Fired(command));
}

#[test]
fn a_malformed_alternative_poisons_the_whole_shortcut() {
    let mut engine = engine(&[CustomCommand::new("bad", "echo bad", "cmd+shift+e|esc")]);
    assert_eq!(engine.advance(&cmd_shift_e()), EngineOutcome::Unmatched);
}

#[test]
fn a_builtin_sequence_fires_its_action() {
    let sequences = BTreeMap::from([(BuiltinAction::ToggleSplit, vec![vec![ctrl_a(), g()]])]);
    let mut engine = CustomCommandEngine::new(&[], &sequences, &BTreeSet::new());
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(
        engine.advance(&g()),
        EngineOutcome::FiredBuiltin(BuiltinAction::ToggleSplit)
    );
    assert!(!engine.is_armed());
}

#[test]
fn builtin_alternatives_and_commands_share_one_matcher() {
    let command = CustomCommand::new("edit", "zed", "cmd+shift+e");
    let sequences = BTreeMap::from([(
        BuiltinAction::ToggleSplit,
        vec![vec![ctrl_a(), g()], vec![ctrl_a(), x()]],
    )]);
    let mut engine = CustomCommandEngine::new(std::slice::from_ref(&command), &sequences, &BTreeSet::new());
    assert_eq!(engine.advance(&cmd_shift_e()), EngineOutcome::Fired(command));
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(
        engine.advance(&x()),
        EngineOutcome::FiredBuiltin(BuiltinAction::ToggleSplit)
    );
}

#[test]
fn the_repeat_flag_from_either_verb_keeps_its_sequence_live() {
    let (keymap, diagnostics) = parse(
        "map ctrl+a>g --repeat next_session\n\
         command \"Grow\" ctrl+a>x --repeat ngtermctl session resize +0.05",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let command = keymap.commands()[0].clone();
    let mut engine = CustomCommandEngine::from_keymap(&keymap);
    assert_eq!(engine.advance(&ctrl_a()), EngineOutcome::Armed);
    assert_eq!(
        engine.advance(&g()),
        EngineOutcome::FiredBuiltin(BuiltinAction::NextSession)
    );
    assert_eq!(
        engine.advance(&g()),
        EngineOutcome::FiredBuiltin(BuiltinAction::NextSession)
    );
    assert_eq!(engine.advance(&x()), EngineOutcome::Fired(command));
    assert!(engine.is_repeating());
}

#[test]
fn a_sequence_without_the_flag_fires_once() {
    let (keymap, _) = parse("map ctrl+a>g next_session");
    let mut engine = CustomCommandEngine::from_keymap(&keymap);
    engine.advance(&ctrl_a());
    assert_eq!(
        engine.advance(&g()),
        EngineOutcome::FiredBuiltin(BuiltinAction::NextSession)
    );
    assert!(!engine.is_repeating());
    assert_eq!(engine.advance(&g()), EngineOutcome::Unmatched);
}
