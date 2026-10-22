//! Ported from upstream's `KeybindTests` (the host-free half).

use uuid::Uuid;

use super::*;
use crate::keymap::keybind::alternative_keybinds;

fn command_target() -> KeybindTarget {
    KeybindTarget::Command(Uuid::new_v4())
}

#[test]
fn shift_function_key_round_trips() {
    let binding = keybind("shift+f6");
    assert_eq!(binding, vec![chord(SHIFT, "f6")]);
    assert_eq!(keybind_display_string(&binding), "shift+f6");
    assert_eq!(parse_keybind(&keybind_display_string(&binding)), Some(binding));
}

#[test]
fn function_keys_round_trip() {
    let expected: Vec<String> = (1..=20).map(|n| format!("f{n}")).collect();
    assert_eq!(FUNCTION_KEYS.to_vec(), expected);
    for name in FUNCTION_KEYS {
        assert!(has_key_position(name));
        assert_eq!(parse_keybind(&name.to_uppercase()), Some(vec![chord(NONE, name)]));
        assert_eq!(parse_keybind(&format!("ctrl+{name}")), Some(vec![chord(CTRL, name)]));
    }
}

#[test]
fn invalid_function_keys_remain_rejected() {
    for token in ["f0", "f21", "f01", "fn+f5"] {
        assert_eq!(parse_keybind(token), None, "{token}");
        assert!(!has_key_position(token), "{token}");
    }
}

#[test]
fn every_bindable_named_key_has_a_key_position() {
    let named: Vec<&str> = bindable_named_keys().collect();
    assert_eq!(named.len(), 28);
    for key in named {
        assert!(has_key_position(key), "{key}");
        assert!(is_bindable_named_key(key), "{key}");
    }
}

#[test]
fn unproducible_keys_have_no_key_position() {
    for key in ["f21", "esc", "", "é", "!", "ю", "ab"] {
        assert!(!has_key_position(key), "{key:?}");
    }
}

#[test]
fn latin_position_keys_cover_every_letter_digit_and_ansi_symbol_and_are_spellable() {
    let symbols = ["=", "-", "]", "[", "'", ";", "\\", ",", "/", ".", "`"];
    let letters_and_digits = ('a'..='z').chain('0'..='9').map(String::from);
    for key in letters_and_digits.chain(symbols.iter().map(|symbol| (*symbol).to_owned())) {
        assert!(has_key_position(&key), "{key}");
        assert_eq!(
            parse_keybind(&format!("cmd+{key}")),
            Some(vec![chord(CMD, &key)]),
            "cmd+{key}"
        );
    }
}

#[test]
fn parse_simple_chord() {
    assert_eq!(parse_keybind("cmd+shift+e"), Some(vec![chord(CMD | SHIFT, "e")]));
}

#[test]
fn parse_bare_key_has_no_modifiers() {
    assert_eq!(parse_keybind("a"), Some(vec![chord(NONE, "a")]));
}

#[test]
fn parse_full_sequence() {
    assert_eq!(
        parse_keybind("ctrl+a>b"),
        Some(vec![chord(CTRL, "a"), chord(NONE, "b")])
    );
}

#[test]
fn parse_sequence_with_modifier_on_each_chord() {
    assert_eq!(
        parse_keybind("ctrl+a>cmd+b"),
        Some(vec![chord(CTRL, "a"), chord(CMD, "b")])
    );
}

#[test]
fn parse_every_modifier_word_variant() {
    for (word, mods) in [
        ("control", CTRL),
        ("ctrl", CTRL),
        ("command", CMD),
        ("cmd", CMD),
        ("option", OPT),
        ("opt", OPT),
        ("alt", OPT),
        ("shift", SHIFT),
    ] {
        assert_eq!(
            parse_keybind(&format!("{word}+x")),
            Some(vec![chord(mods, "x")]),
            "{word}"
        );
    }
}

#[test]
fn parse_all_modifiers_together() {
    assert_eq!(
        parse_keybind("ctrl+cmd+opt+shift+k"),
        Some(vec![chord(CTRL | CMD | OPT | SHIFT, "k")])
    );
}

#[test]
fn parse_is_case_insensitive() {
    assert_eq!(parse_keybind("CMD+Shift+E"), Some(vec![chord(CMD | SHIFT, "e")]));
    assert_eq!(
        parse_keybind("CTRL+A>B"),
        Some(vec![chord(CTRL, "a"), chord(NONE, "b")])
    );
}

#[test]
fn parse_trims_whitespace_around_tokens() {
    assert_eq!(
        parse_keybind("ctrl + a > b"),
        Some(vec![chord(CTRL, "a"), chord(NONE, "b")])
    );
}

#[test]
fn parse_named_key() {
    assert_eq!(parse_keybind("ctrl+space"), Some(vec![chord(CTRL, "space")]));
}

#[test]
fn parse_arrow_keys() {
    assert_eq!(parse_keybind("cmd+shift+left"), Some(vec![chord(CMD | SHIFT, "left")]));
    assert_eq!(
        parse_keybind("cmd+shift+right"),
        Some(vec![chord(CMD | SHIFT, "right")])
    );
    assert_eq!(parse_keybind("cmd+opt+up"), Some(vec![chord(CMD | OPT, "up")]));
    assert_eq!(parse_keybind("ctrl+opt+down"), Some(vec![chord(CTRL | OPT, "down")]));
}

#[test]
fn parse_arrow_keys_is_case_insensitive_and_works_bare_and_in_leaders() {
    assert_eq!(parse_keybind("CMD+SHIFT+LEFT"), Some(vec![chord(CMD | SHIFT, "left")]));
    // a bare arrow parses; the modifier requirement is a `map` rule, not a grammar one.
    assert_eq!(parse_keybind("up"), Some(vec![chord(NONE, "up")]));
    assert_eq!(
        parse_keybind("ctrl+a>left"),
        Some(vec![chord(CTRL, "a"), chord(NONE, "left")])
    );
}

#[test]
fn parse_still_rejects_named_keys_the_runner_cannot_produce() {
    for token in [
        "cmd+esc",
        "cmd+f21",
        "cmd+home",
        "cmd+end",
        "cmd+pageup",
        "cmd+arrowleft",
    ] {
        assert_eq!(parse_keybind(token), None, "{token}");
    }
}

#[test]
fn arrow_keys_are_a_subset_of_the_named_keys() {
    assert_eq!(ARROW_KEYS, ["left", "right", "up", "down"]);
    assert!(
        ARROW_KEYS
            .iter()
            .all(|key| is_bindable_named_key(key) && is_arrow_key(key))
    );
}

#[test]
fn parse_rejects_malformed_chords() {
    for token in [
        "",
        "ctrl+",
        "+a",
        "ctrl+a>",
        "ctrl+cmd",
        "a+b",
        "fn+ctrl+a",
        "super+x",
        "hyper+x",
    ] {
        assert_eq!(parse_keybind(token), None, "{token:?}");
    }
}

#[test]
fn no_conflicts_for_distinct_shortcuts() {
    let binds = [
        (vec![chord(CMD, "a")], command_target()),
        (vec![chord(CMD, "b")], command_target()),
    ];
    assert!(keybind_conflicts(&binds).is_empty());
}

#[test]
fn detects_duplicate_shortcut() {
    let (a, b) = (command_target(), command_target());
    let cmd_a = vec![chord(CMD, "a")];
    let conflicts = keybind_conflicts(&[(cmd_a.clone(), a), (cmd_a.clone(), b)]);
    assert_eq!(
        conflicts,
        vec![KeybindConflict {
            first: ConflictSide {
                target: a,
                keybind: cmd_a.clone(),
            },
            second: ConflictSide {
                target: b,
                keybind: cmd_a,
            },
        }]
    );
}

#[test]
fn detects_prefix_overlap_in_either_order() {
    let (leader, sequence) = (command_target(), command_target());
    let short = vec![chord(CTRL, "a")];
    let long = vec![chord(CTRL, "a"), chord(NONE, "b")];
    let forward = keybind_conflicts(&[(short.clone(), leader), (long.clone(), sequence)]);
    assert_eq!(forward.len(), 1);
    assert_eq!((forward[0].first.target, forward[0].second.target), (leader, sequence));
    let reversed = keybind_conflicts(&[(long.clone(), sequence), (short, leader)]);
    assert_eq!(reversed.len(), 1);
    assert_eq!(
        (reversed[0].first.keybind.clone(), reversed[0].second.target),
        (long, leader)
    );
}

#[test]
fn detects_three_way_overlap() {
    let binds = [
        (keybind("ctrl+a"), command_target()),
        (keybind("ctrl+a>b"), command_target()),
        (keybind("ctrl+a>b>c"), command_target()),
    ];
    let pairs: Vec<(KeybindTarget, KeybindTarget)> = keybind_conflicts(&binds)
        .into_iter()
        .map(|conflict| (conflict.first.target, conflict.second.target))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (binds[0].1, binds[1].1),
            (binds[0].1, binds[2].1),
            (binds[1].1, binds[2].1)
        ]
    );
}

#[test]
fn sibling_sequences_sharing_a_leader_do_not_conflict() {
    let binds = [
        (keybind("ctrl+a>b"), command_target()),
        (keybind("ctrl+a>c"), command_target()),
    ];
    assert!(keybind_conflicts(&binds).is_empty());
}

#[test]
fn detects_a_conflict_between_command_and_builtin_alternatives() {
    let command = command_target();
    let builtin = KeybindTarget::Builtin(BuiltinAction::ToggleSplit);
    let conflicts = keybind_conflicts(&[(keybind("ctrl+a>b"), command), (keybind("ctrl+a"), builtin)]);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        (conflicts[0].first.target, conflicts[0].second.target),
        (command, builtin)
    );
}

#[test]
fn alternatives_of_one_target_conflict_with_each_other() {
    let target = command_target();
    let conflicts = keybind_conflicts(&[(keybind("ctrl+a"), target), (keybind("ctrl+a>b"), target)]);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        (conflicts[0].first.target, conflicts[0].second.target),
        (target, target)
    );
}

#[test]
fn strict_prefix_is_shorter_and_leading() {
    assert!(is_strict_keybind_prefix(&keybind("ctrl+a"), &keybind("ctrl+a>b")));
    assert!(!is_strict_keybind_prefix(&keybind("ctrl+a"), &keybind("ctrl+a")));
    assert!(!is_strict_keybind_prefix(&keybind("ctrl+a>b"), &keybind("ctrl+a")));
    assert!(!is_strict_keybind_prefix(&keybind("ctrl+b"), &keybind("ctrl+a>b")));
}

#[test]
fn reserved_monitor_chords_match_the_monitor_predicates() {
    // the switcher checks that Control is held, so Tab is reserved under any extra modifiers.
    for mods in [CTRL, CTRL | SHIFT, CTRL | OPT, CTRL | CMD] {
        assert!(is_reserved_monitor_chord(&chord(mods, "tab")));
    }
    // the pane shortcuts take 1/2 only when Control is the sole modifier.
    assert!(is_reserved_monitor_chord(&chord(CTRL, "1")));
    assert!(is_reserved_monitor_chord(&chord(CTRL, "2")));
}

#[test]
fn non_monitor_chords_are_not_reserved() {
    for (mods, key) in [
        (CMD, "tab"),
        (NONE, "tab"),
        (CTRL | SHIFT, "1"),
        (CTRL | CMD, "2"),
        (CTRL, "3"),
        (CMD | SHIFT, "e"),
    ] {
        assert!(!is_reserved_monitor_chord(&chord(mods, key)), "{mods:?}+{key}");
    }
}

#[test]
fn chord_display_string_round_trips_kitty_syntax() {
    assert_eq!(chord(CMD | SHIFT, "e").display_string(), "cmd+shift+e");
    assert_eq!(chord(NONE, "a").display_string(), "a");
    assert_eq!(chord(CTRL, "tab").display_string(), "ctrl+tab");
    // the fixed ctrl+cmd+opt+shift order, whatever order the flags were combined in.
    assert_eq!(
        chord(SHIFT | OPT | CMD | CTRL, "x").display_string(),
        "ctrl+cmd+opt+shift+x"
    );
    let new_window = chord(CMD | OPT, "n");
    assert_eq!(parse_keybind(&new_window.display_string()), Some(vec![new_window]));
    assert_eq!(modifier_words(CTRL | SHIFT).collect::<Vec<_>>(), ["ctrl", "shift"]);
}

#[test]
fn arrow_chords_round_trip_through_the_display_string() {
    for key in ARROW_KEYS {
        let arrow = chord(CMD | SHIFT, key);
        assert_eq!(arrow.display_string(), format!("cmd+shift+{key}"));
        assert_eq!(parse_keybind(&arrow.display_string()), Some(vec![arrow]));
    }
}

#[test]
fn parse_keybinds_without_a_pipe_wraps_the_single_result() {
    assert_eq!(parse_keybinds("cmd+shift+e"), Some(vec![vec![chord(CMD | SHIFT, "e")]]));
    assert_eq!(
        parse_keybinds("ctrl+a>g"),
        Some(vec![vec![chord(CTRL, "a"), chord(NONE, "g")]])
    );
}

#[test]
fn parse_keybinds_splits_alternatives() {
    assert_eq!(
        parse_keybinds("cmd+t|ctrl+space"),
        Some(vec![vec![chord(CMD, "t")], vec![chord(CTRL, "space")]])
    );
    assert_eq!(
        parse_keybinds("cmd+t|ctrl+t|opt+t"),
        Some(vec![
            vec![chord(CMD, "t")],
            vec![chord(CTRL, "t")],
            vec![chord(OPT, "t")]
        ])
    );
    assert_eq!(
        parse_keybinds("cmd+t|ctrl+space>s"),
        Some(vec![
            vec![chord(CMD, "t")],
            vec![chord(CTRL, "space"), chord(NONE, "s")]
        ])
    );
    assert_eq!(
        parse_keybinds("ctrl+a>m|cmd+a>m"),
        Some(vec![
            vec![chord(CTRL, "a"), chord(NONE, "m")],
            vec![chord(CMD, "a"), chord(NONE, "m")]
        ])
    );
}

#[test]
fn parse_keybinds_rejects_empty_alternatives() {
    for token in ["", "a||b", "|cmd+t", "cmd+t|", "|"] {
        assert_eq!(parse_keybinds(token), None, "{token:?}");
    }
}

#[test]
fn parse_keybinds_rejects_the_whole_list_when_one_alternative_is_malformed() {
    for token in ["cmd+t|f21", "f21|cmd+t", "cmd+t|ctrl+", "cmd+t|cmd+a+b"] {
        assert_eq!(parse_keybinds(token), None, "{token}");
    }
}

#[test]
fn parse_keybinds_rejects_a_pipe_as_a_base_key() {
    // no unshifted key produces `|`; the spelling that fires is shift+\.
    assert_eq!(parse_keybinds("cmd+|"), None);
}

#[test]
fn keybind_display_string_joins_chords_with_an_angle() {
    let leader = vec![chord(CTRL, "a"), chord(NONE, "g")];
    assert_eq!(keybind_display_string(&leader), "ctrl+a>g");
    assert_eq!(parse_keybind(&keybind_display_string(&leader)), Some(leader));
    assert_eq!(keybind_display_string(&[chord(CMD, "t")]), "cmd+t");
}

/// The raw spelling the surviving alternatives are stored under, spliced back together.
fn raw_alternatives(token: &str) -> Option<String> {
    alternative_keybinds(token).map(|alternatives| {
        alternatives
            .iter()
            .map(|alternative| alternative.raw.as_str())
            .collect::<Vec<_>>()
            .join("|")
    })
}

#[test]
fn alternative_keybinds_drops_a_repeat_and_keeps_the_survivors_raw_spelling() {
    assert_eq!(raw_alternatives("cmd+t|cmd+t").as_deref(), Some("cmd+t"));
    assert_eq!(
        raw_alternatives("cmd+t|ctrl+space>s|cmd+t").as_deref(),
        Some("cmd+t|ctrl+space>s")
    );
    assert_eq!(
        raw_alternatives("command+shift+a|cmd+shift+a").as_deref(),
        Some("command+shift+a")
    );
    assert_eq!(raw_alternatives("CMD+T|cmd+t").as_deref(), Some("CMD+T"));
}

#[test]
fn alternative_keybinds_leaves_a_distinct_list_untouched() {
    assert_eq!(raw_alternatives("cmd+t").as_deref(), Some("cmd+t"));
    assert_eq!(raw_alternatives("cmd+t|ctrl+t").as_deref(), Some("cmd+t|ctrl+t"));
    assert_eq!(raw_alternatives("a|b echo"), None, "a shell tail is no binding at all");
}

#[test]
fn a_malformed_alternative_is_told_apart_from_a_shell_pipeline() {
    assert!(has_malformed_alternative("cmd+e|f21"));
    assert!(has_malformed_alternative("f21|cmd+e"));
    assert!(
        !has_malformed_alternative("ls|grep"),
        "neither half is a keybind, so this is a pipeline"
    );
    assert!(
        !has_malformed_alternative("f21"),
        "a lone token carries no alternative to be malformed"
    );
}
