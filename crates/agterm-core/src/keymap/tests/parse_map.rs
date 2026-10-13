//! Ported from upstream's `KeymapTests`: the `map` verb, its alternatives and the built-in collision fixpoint.

use super::*;

use BuiltinAction as A;

#[test]
fn rebinding_toggle_search_resolves_through_the_generic_path() {
    let (keymap, diagnostics) = parse("map cmd+shift+l toggle_search");
    assert!(diagnostics.is_empty());
    let override_chord = chord(CMD | SHIFT, "l");
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSearch, override_chord.clone())])
    );
    assert_eq!(keymap.equivalent(A::ToggleSearch), Some(&override_chord));
    assert_eq!(default_chord(A::ToggleSearch), Some(chord(CMD, "f")));
}

#[test]
fn an_unknown_action_is_diagnosed() {
    let (keymap, diagnostics) = parse("map cmd+d not_an_action");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(diagnostics, [diagnostic(1, "unknown action 'not_an_action'")]);
}

#[test]
fn a_leader_on_a_builtin_binds_the_monitor_and_unbinds_the_menu() {
    let (keymap, diagnostics) = parse("map ctrl+a>g toggle_split");
    assert!(diagnostics.is_empty());
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "a"), chord(NONE, "g")]]
    );
    // the line declares the whole binding, so the default must not stay live behind the sequence.
    assert_eq!(keymap.equivalent(A::ToggleSplit), None);
    assert_eq!(
        keymap.builtin_unbound().iter().copied().collect::<Vec<_>>(),
        [A::ToggleSplit]
    );
}

#[test]
fn an_invalid_chord_is_diagnosed() {
    let (keymap, diagnostics) = parse("map cmd+f21 toggle_split");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(diagnostics, [diagnostic(1, "invalid chord 'cmd+f21'")]);
}

#[test]
fn arrow_chord_map_lines_parse() {
    let (keymap, diagnostics) = parse("map cmd+shift+left previous_session\nmap cmd+shift+right next_session");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::PreviousSession), Some(&chord(CMD | SHIFT, "left")));
    assert_eq!(keymap.equivalent(A::NextSession), Some(&chord(CMD | SHIFT, "right")));
}

#[test]
fn a_bare_arrow_map_is_rejected() {
    let (keymap, diagnostics) = parse("map left previous_session");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(keymap.equivalent(A::PreviousSession), Some(&chord(CMD | OPT, "up")));
    // exact: a single-alternative rejection carries no scope suffix.
    assert_eq!(
        diagnostics,
        [diagnostic(1, "bare arrow chord 'left' needs a modifier; map skipped")]
    );
}

#[test]
fn a_modified_arrow_map_is_accepted_for_every_arrow() {
    for key in ARROW_KEYS {
        let (keymap, diagnostics) = parse(&format!("map cmd+shift+{key} new_session"));
        assert!(diagnostics.is_empty(), "unexpected diagnostics for cmd+shift+{key}");
        assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD | SHIFT, key)));
    }
}

#[test]
fn a_map_onto_an_arrow_actions_unmoved_default_is_rejected() {
    let (keymap, diagnostics) = parse("map cmd+opt+up new_session");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "n")));
    assert_eq!(keymap.equivalent(A::PreviousSession), Some(&chord(CMD | OPT, "up")));
    assert_eq!(
        diagnostics,
        [diagnostic(
            1,
            "chord conflicts with built-in 'previous_session'; map skipped"
        )]
    );
}

#[test]
fn a_bare_non_arrow_map_is_still_accepted() {
    let (keymap, diagnostics) = parse("map a new_session");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(NONE, "a")));
    assert!(keymap.sequences(A::NewSession).is_empty());
    assert!(keymap.builtin_unbound().is_empty());
}

#[test]
fn map_alternatives_split_between_menu_and_monitor() {
    let (keymap, diagnostics) = parse("map cmd+t|ctrl+space>s toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "space"), chord(NONE, "s")]]
    );
    assert!(keymap.builtin_unbound().is_empty());
}

#[test]
fn two_single_chord_alternatives_put_the_first_on_the_menu() {
    let (keymap, diagnostics) = parse("map cmd+t|cmd+y toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert_eq!(keymap.sequences(A::ToggleSplit), [vec![chord(CMD, "y")]]);
}

#[test]
fn three_alternatives_keep_the_monitor_ones_in_line_order() {
    let (keymap, diagnostics) = parse("map cmd+t|ctrl+space>s|cmd+ctrl+y toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [keybind("ctrl+space>s"), keybind("cmd+ctrl+y")]
    );
}

#[test]
fn a_sequence_only_line_leaves_the_action_without_a_menu_chord() {
    let (keymap, diagnostics) = parse("map ctrl+space>s toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), None);
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "space"), chord(NONE, "s")]]
    );
    // an untouched action keeps its shipped default: only the mapped one goes unbound.
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "n")));
}

#[test]
fn a_bare_first_chord_on_a_monitor_alternative_is_rejected() {
    let (keymap, diagnostics) = parse("map cmd+t|y>s toggle_split");
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(
        messages(&diagnostics),
        ["chord 'y>s' needs a modifier on its first key; alternative skipped"]
    );
}

#[test]
fn a_map_whose_only_alternative_is_shadowed_by_a_builtin_keeps_the_shipped_default() {
    let (keymap, diagnostics) = parse("map ctrl+a new_session\nmap ctrl+a>s toggle_split");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert!(!keymap.builtin_unbound().contains(&A::ToggleSplit));
    assert_eq!(
        messages(&diagnostics),
        ["built-in 'toggle_split' chord 'ctrl+a>s' conflicts with a built-in; keybind dropped"]
    );
}

#[test]
fn a_map_whose_only_alternative_loses_a_conflict_keeps_the_shipped_default() {
    let (keymap, diagnostics) = parse("map ctrl+a>s toggle_split\ncommand \"X\" ctrl+a>s echo x");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(diagnostics.len(), 2);
}

#[test]
fn a_stranded_action_stays_unbound_when_another_builtin_took_its_default() {
    let (keymap, diagnostics) = parse("map ctrl+a>d toggle_split\nmap cmd+d new_session\nmap ctrl+a quick_terminal");
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "d")));
    assert_eq!(keymap.equivalent(A::ToggleSplit), None);
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(
        messages(&diagnostics),
        ["built-in 'toggle_split' chord 'ctrl+a>d' conflicts with a built-in; keybind dropped"]
    );
}

#[test]
fn a_map_whose_menu_chord_collides_keeps_its_alternatives_and_says_so() {
    let (keymap, diagnostics) = parse("map cmd+w|ctrl+a>w toggle_split");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "a"), chord(NONE, "w")]]
    );
    assert_eq!(
        messages(&diagnostics),
        ["chord conflicts with built-in 'close_session'; alternative skipped"]
    );
}

#[test]
fn a_map_with_no_alternatives_left_still_reports_the_line_as_skipped() {
    let (keymap, diagnostics) = parse("map cmd+w toggle_split");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert_eq!(
        messages(&diagnostics),
        ["chord conflicts with built-in 'close_session'; map skipped"]
    );
}

#[test]
fn a_map_with_only_a_bare_sequence_keeps_the_shipped_default() {
    let (keymap, diagnostics) = parse("map y>s toggle_split");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert!(keymap.builtin_unbound().is_empty());
    assert_eq!(
        messages(&diagnostics),
        ["chord 'y>s' needs a modifier on its first key; map skipped"]
    );
}

#[test]
fn a_reserved_alternative_drops_alone_and_leaves_the_sibling_live() {
    let (keymap, diagnostics) = parse("map ctrl+1|ctrl+a>s toggle_split");
    assert_eq!(keymap.equivalent(A::ToggleSplit), None);
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "a"), chord(NONE, "s")]]
    );
    assert_eq!(
        messages(&diagnostics),
        ["chord 'ctrl+1' is a reserved shortcut; alternative skipped"]
    );
}

#[test]
fn a_reserved_chord_inside_a_sequence_alternative_is_rejected() {
    let (keymap, diagnostics) = parse("map cmd+t|ctrl+a>ctrl+tab toggle_split");
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(
        messages(&diagnostics),
        ["chord 'ctrl+a>ctrl+tab' is a reserved shortcut; alternative skipped"]
    );
}

#[test]
fn a_bare_arrow_alternative_is_rejected_while_the_sibling_survives() {
    let (keymap, diagnostics) = parse("map left|cmd+opt+shift+left previous_session");
    assert_eq!(
        keymap.equivalent(A::PreviousSession),
        Some(&chord(CMD | OPT | SHIFT, "left"))
    );
    assert_eq!(
        messages(&diagnostics),
        ["bare arrow chord 'left' needs a modifier; alternative skipped"]
    );
}

#[test]
fn identical_alternatives_are_deduped() {
    let (keymap, diagnostics) = parse("map cmd+t|cmd+t toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
}

#[test]
fn a_malformed_alternative_poisons_the_whole_line() {
    let (keymap, diagnostics) = parse("map cmd+t|cmd+f21 toggle_split");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(messages(&diagnostics), ["invalid chord 'cmd+t|cmd+f21'"]);
}

#[test]
fn a_later_map_line_replaces_the_whole_earlier_binding() {
    let (keymap, diagnostics) = parse("map cmd+t|ctrl+space>s toggle_split\nmap ctrl+a>d toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), None);
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "a"), chord(NONE, "d")]]
    );
}

#[test]
fn a_later_map_line_drops_the_earlier_sequences() {
    let (keymap, diagnostics) = parse("map ctrl+a>d toggle_split\nmap cmd+t toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert!(keymap.builtin_unbound().is_empty());
}

#[test]
fn alternatives_losing_their_menu_chord_to_a_collision_keep_their_sequences() {
    let (keymap, diagnostics) = parse("map cmd+d|ctrl+a>n new_session");
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "n")));
    assert_eq!(
        keymap.sequences(A::NewSession),
        [vec![chord(CTRL, "a"), chord(NONE, "n")]]
    );
    assert_eq!(
        messages(&diagnostics),
        ["chord conflicts with built-in 'toggle_split'; alternative skipped"]
    );
}

#[test]
fn a_restored_default_shadowing_its_own_alternative_keeps_the_generic_diagnostic() {
    let (keymap, diagnostics) = parse("map cmd+o|cmd+d toggle_split");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(
        messages(&diagnostics),
        [
            "chord conflicts with built-in 'open_directory'; alternative skipped",
            "built-in 'toggle_split' chord 'cmd+d' conflicts with a built-in; alternative dropped",
        ]
    );
}

#[test]
fn an_unbound_action_frees_its_default_chord_for_a_custom_command() {
    let (keymap, diagnostics) = parse("map ctrl+a>d toggle_split\ncommand \"Split\" cmd+d echo split");
    assert!(diagnostics.is_empty());
    assert_eq!(shortcuts(&keymap), ["cmd+d"]);
}

#[test]
fn an_unbound_action_frees_its_default_chord_for_another_builtin() {
    let (keymap, diagnostics) = parse("map ctrl+a>d toggle_split\nmap cmd+d new_session");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.builtin_overrides().get(&A::NewSession), Some(&chord(CMD, "d")));
    assert_eq!(keymap.equivalent(A::ToggleSplit), None);
    assert_eq!(
        keymap.sequences(A::ToggleSplit),
        [vec![chord(CTRL, "a"), chord(NONE, "d")]]
    );
}

#[test]
fn colliding_builtin_overrides_both_drop() {
    let (keymap, diagnostics) = parse("map cmd+shift+e toggle_split\nmap cmd+shift+e new_session");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert_eq!(keymap.equivalent(A::NewSession).cloned(), default_chord(A::NewSession));
    assert_eq!(
        diagnostics,
        [
            diagnostic(1, "chord conflicts with built-in 'new_session'; map skipped"),
            diagnostic(2, "chord conflicts with built-in 'toggle_split'; map skipped"),
        ]
    );
}

#[test]
fn a_map_to_another_builtins_unmoved_default_is_rejected() {
    let (keymap, diagnostics) = parse("map cmd+d new_session");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "n")));
    assert_eq!(
        diagnostics,
        [diagnostic(
            1,
            "chord conflicts with built-in 'toggle_split'; map skipped"
        )]
    );
}

#[test]
fn a_freed_default_is_takeable_in_either_line_order() {
    for text in [
        "map cmd+shift+e toggle_split\nmap cmd+d new_session",
        "map cmd+d new_session\nmap cmd+shift+e toggle_split",
    ] {
        let (keymap, diagnostics) = parse(text);
        assert!(diagnostics.is_empty(), "{text}");
        assert_eq!(
            keymap.builtin_overrides().get(&A::ToggleSplit),
            Some(&chord(CMD | SHIFT, "e"))
        );
        assert_eq!(keymap.builtin_overrides().get(&A::NewSession), Some(&chord(CMD, "d")));
    }
}

#[test]
fn a_tab_separated_map_line_parses() {
    let (keymap, diagnostics) = parse("map\tcmd+shift+e\ttoggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
}

#[test]
fn a_cascading_collision_drops_both_and_reverts_to_defaults() {
    // toggle_split's cmd+o loses to open_directory's unmoved default, reverts to cmd+d, and that collides with
    // new_session's cmd+d: resolution must iterate to a fixpoint and drop both.
    let (keymap, diagnostics) = parse("map cmd+o toggle_split\nmap cmd+d new_session");
    assert!(keymap.builtin_overrides().is_empty());
    for action in [A::ToggleSplit, A::NewSession, A::OpenDirectory] {
        assert_eq!(keymap.equivalent(action).cloned(), default_chord(action));
    }
    let chords: Vec<&Chord> = BuiltinAction::all()
        .filter_map(|action| keymap.equivalent(action))
        .collect();
    let distinct: std::collections::HashSet<&&Chord> = chords.iter().collect();
    assert_eq!(chords.len(), distinct.len());
    assert_eq!(
        diagnostics,
        [
            diagnostic(1, "chord conflicts with built-in 'open_directory'; map skipped"),
            diagnostic(2, "chord conflicts with built-in 'toggle_split'; map skipped"),
        ]
    );
}

#[test]
fn a_builtin_mapped_to_a_reserved_monitor_chord_is_rejected() {
    let (keymap, diagnostics) = parse("map ctrl+1 new_session");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "n")));
    assert_eq!(
        diagnostics,
        [diagnostic(1, "chord 'ctrl+1' is a reserved shortcut; map skipped")]
    );

    let (keymap, diagnostics) = parse("map ctrl+tab quick_terminal");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(
        messages(&diagnostics),
        ["chord 'ctrl+tab' is a reserved shortcut; map skipped"]
    );
}

#[test]
fn mapping_the_same_action_twice_is_last_wins() {
    let (keymap, diagnostics) = parse("map cmd+shift+e toggle_split\nmap cmd+shift+x toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "x"))])
    );
}

#[test]
fn a_map_line_missing_its_action_is_diagnosed() {
    for text in ["map cmd+d", "map", "map cmd+d --repeat"] {
        let (keymap, diagnostics) = parse(text);
        assert!(keymap.builtin_overrides().is_empty());
        assert_eq!(messages(&diagnostics), ["map requires a chord and an action"], "{text}");
    }
}

#[test]
fn an_action_name_with_trailing_words_is_unknown() {
    let (_, diagnostics) = parse("map cmd+shift+e toggle_split now");
    assert_eq!(messages(&diagnostics), ["unknown action 'toggle_split now'"]);
}
