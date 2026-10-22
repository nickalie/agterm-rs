//! Ported from upstream's `KeymapTests`: the cross-section validation of every monitor-bound alternative, its
//! order independence, the pipe-free compatibility invariant and the conformance fixtures.

use super::*;

use BuiltinAction as A;

/// Every binding the keymap ended up with, in an order derived from the model rather than the file, so two
/// spellings of the same set compare equal.
fn binding_summary(keymap: &Keymap) -> Vec<String> {
    let builtins = BuiltinAction::all().filter_map(|action| {
        let binds: Vec<String> = keymap
            .equivalent(action)
            .map(Chord::display_string)
            .into_iter()
            .chain(
                keymap
                    .sequences(action)
                    .iter()
                    .map(|keybind| keybind_display_string(keybind)),
            )
            .collect();
        (!binds.is_empty()).then(|| format!("{}={}", action.name(), binds.join("|")))
    });
    let mut commands: Vec<String> = keymap
        .commands()
        .iter()
        .map(|command| format!("{}={}", command.name, command.shortcut))
        .collect();
    commands.sort();
    builtins.chain(commands).collect()
}

fn sorted_messages(diagnostics: &[KeymapDiagnostic]) -> Vec<&str> {
    let mut messages = messages(diagnostics);
    messages.sort_unstable();
    messages
}

#[test]
fn a_custom_command_on_an_arrow_default_is_dropped() {
    let (keymap, diagnostics) = parse("command \"Nav\" cmd+opt+down echo hi");
    assert_eq!(keymap.commands()[0].name, "Nav");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(
        diagnostics,
        [diagnostic(
            0,
            "custom command 'Nav' shortcut 'cmd+opt+down' conflicts with a built-in; keybind dropped"
        )]
    );
}

#[test]
fn colliding_builtin_line_order_cannot_silently_change_a_custom_shortcut() {
    let forward = parse("map cmd+shift+x toggle_split\nmap cmd+shift+x new_session\ncommand \"C\" cmd+d echo c");
    let reversed = parse("map cmd+shift+x new_session\nmap cmd+shift+x toggle_split\ncommand \"C\" cmd+d echo c");
    for (keymap, diagnostics) in [&forward, &reversed] {
        assert!(keymap.builtin_overrides().is_empty());
        assert_eq!(
            keymap.equivalent(A::ToggleSplit).cloned(),
            default_chord(A::ToggleSplit)
        );
        assert_eq!(keymap.equivalent(A::NewSession).cloned(), default_chord(A::NewSession));
        assert_eq!(shortcuts(keymap), [""]);
        assert_eq!(diagnostics.len(), 3);
        assert_eq!(
            diagnostics.last(),
            Some(&diagnostic(
                0,
                "custom command 'C' shortcut 'cmd+d' conflicts with built-in 'toggle_split'; keybind dropped"
            ))
        );
    }
    assert_eq!(binding_summary(&reversed.0), binding_summary(&forward.0));
}

#[test]
fn a_custom_chord_equal_to_a_builtin_default_is_dropped() {
    let (keymap, diagnostics) = parse("command \"Boom\" cmd+d echo boom");
    assert_eq!(keymap.commands()[0].command, "echo boom");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(
        diagnostics,
        [diagnostic(
            0,
            "custom command 'Boom' shortcut 'cmd+d' conflicts with a built-in; keybind dropped"
        )]
    );
}

#[test]
fn a_custom_chord_equal_to_an_overridden_builtin_chord_is_dropped() {
    let (keymap, diagnostics) = parse("map cmd+shift+e toggle_split\ncommand \"Boom\" cmd+shift+e echo boom");
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(
        messages(&diagnostics),
        ["custom command 'Boom' shortcut 'cmd+shift+e' conflicts with a built-in; keybind dropped"]
    );
}

#[test]
fn a_freed_default_chord_is_usable_by_a_custom_command_after_the_builtin_moves() {
    let (keymap, diagnostics) = parse("command \"Boom\" cmd+d echo boom\nmap cmd+shift+e toggle_split");
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert_eq!(shortcuts(&keymap), ["cmd+d"]);
    assert!(diagnostics.is_empty());
}

#[test]
fn a_custom_leader_starting_with_a_builtin_chord_is_dropped() {
    let (keymap, diagnostics) = parse("command \"Boom\" cmd+d>g echo boom");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(
        messages(&diagnostics),
        ["custom command 'Boom' shortcut 'cmd+d>g' conflicts with a built-in; keybind dropped"]
    );
}

#[test]
fn a_custom_binding_holding_a_reserved_chord_anywhere_is_dropped() {
    for shortcut in ["ctrl+1", "ctrl+tab>g", "ctrl+a>ctrl+1"] {
        let (keymap, diagnostics) = parse(&format!("command \"Boom\" {shortcut} echo boom"));
        assert_eq!(keymap.commands()[0].command, "echo boom");
        assert_eq!(shortcuts(&keymap), [""]);
        let expected =
            format!("custom command 'Boom' shortcut '{shortcut}' conflicts with a reserved shortcut; keybind dropped");
        assert_eq!(diagnostics, [diagnostic(0, &expected)], "{shortcut}");
    }
}

#[test]
fn a_custom_leader_with_no_reserved_chord_is_accepted() {
    let (keymap, diagnostics) = parse("command \"Lazygit\" ctrl+a>g lazygit");
    assert_eq!(shortcuts(&keymap), ["ctrl+a>g"]);
    assert!(diagnostics.is_empty());
}

#[test]
fn custom_versus_custom_duplicates_and_prefixes_drop_both() {
    for (text, first, second) in [
        (
            "command \"First\" cmd+shift+e echo one\ncommand \"Second\" cmd+shift+e echo two",
            "First",
            "Second",
        ),
        (
            "command \"Lead\" ctrl+a echo lead\ncommand \"Leader\" ctrl+a>g echo leader",
            "Lead",
            "Leader",
        ),
    ] {
        let (keymap, diagnostics) = parse(text);
        assert_eq!(shortcuts(&keymap), ["", ""]);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].message.starts_with(&format!("custom command '{first}'")));
        assert!(
            diagnostics[0]
                .message
                .contains(&format!("conflicts with custom command '{second}'"))
        );
        assert!(
            diagnostics[1]
                .message
                .starts_with(&format!("custom command '{second}'"))
        );
        assert!(
            diagnostics[1]
                .message
                .contains(&format!("conflicts with custom command '{first}'"))
        );
    }
}

#[test]
fn a_command_alternative_shadowed_by_a_builtin_drops_alone_and_keeps_the_sibling() {
    let (keymap, diagnostics) = parse("command \"Boom\" cmd+d|ctrl+a>g echo boom");
    assert_eq!(shortcuts(&keymap), ["ctrl+a>g"]);
    assert_eq!(
        diagnostics,
        [diagnostic(
            0,
            "custom command 'Boom' shortcut 'cmd+d' conflicts with a built-in; alternative dropped"
        )]
    );
}

#[test]
fn a_command_with_every_alternative_shadowed_ends_up_palette_only() {
    let (keymap, diagnostics) = parse("command \"Boom\" cmd+d|cmd+n echo boom");
    assert_eq!(keymap.commands()[0].command, "echo boom");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(
        messages(&diagnostics),
        [
            "custom command 'Boom' shortcut 'cmd+d' conflicts with a built-in; alternative dropped",
            "custom command 'Boom' shortcut 'cmd+n' conflicts with a built-in; alternative dropped",
        ]
    );
}

#[test]
fn a_command_alternative_keeps_its_raw_spelling_when_a_sibling_drops() {
    let (keymap, _) = parse("command \"Boom\" cmd+d|COMMAND+Shift+A echo boom");
    assert_eq!(shortcuts(&keymap), ["COMMAND+Shift+A"]);
}

#[test]
fn a_builtin_alternative_shadowed_by_its_own_menu_chord_is_dropped() {
    let (keymap, diagnostics) = parse("map cmd+t|cmd+t>s toggle_split");
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(
        diagnostics,
        [diagnostic(
            1,
            "built-in 'toggle_split' chord 'cmd+t>s' conflicts with a built-in; alternative dropped"
        )]
    );
}

#[test]
fn a_command_alternative_conflicting_with_a_builtin_alternative_loses_only_that_key() {
    let (keymap, diagnostics) = parse("map cmd+t|ctrl+a>s toggle_split\ncommand \"Boom\" ctrl+a>s|ctrl+b>s echo boom");
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert_eq!(shortcuts(&keymap), ["ctrl+b>s"]);
    assert_eq!(
        diagnostics,
        [
            diagnostic(
                0,
                "custom command 'Boom' shortcut 'ctrl+a>s' conflicts with built-in 'toggle_split'; alternative dropped"
            ),
            diagnostic(
                1,
                "built-in 'toggle_split' chord 'ctrl+a>s' conflicts with custom command 'Boom'; alternative dropped"
            ),
        ]
    );
}

#[test]
fn builtin_versus_builtin_alternatives_drop_both_and_keep_their_menu_chords() {
    let (keymap, diagnostics) = parse("map cmd+t|ctrl+a>s toggle_split\nmap cmd+y|ctrl+a>s new_session");
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "t")));
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "y")));
    assert!(keymap.sequences(A::ToggleSplit).is_empty());
    assert!(keymap.sequences(A::NewSession).is_empty());
    assert_eq!(
        diagnostics,
        [
            diagnostic(
                1,
                "built-in 'toggle_split' chord 'ctrl+a>s' conflicts with built-in 'new_session'; alternative dropped"
            ),
            diagnostic(
                2,
                "built-in 'new_session' chord 'ctrl+a>s' conflicts with built-in 'toggle_split'; alternative dropped"
            ),
        ]
    );
}

#[test]
fn a_prefix_conflict_between_alternatives_drops_both_sides_and_keeps_siblings() {
    let (keymap, diagnostics) =
        parse("command \"Lead\" ctrl+space|cmd+y echo lead\ncommand \"Seq\" ctrl+space>s|cmd+u echo seq");
    assert_eq!(shortcuts(&keymap), ["cmd+y", "cmd+u"]);
    assert_eq!(
        messages(&diagnostics),
        [
            "custom command 'Lead' shortcut 'ctrl+space' conflicts with custom command 'Seq'; alternative dropped",
            "custom command 'Seq' shortcut 'ctrl+space>s' conflicts with custom command 'Lead'; alternative dropped",
        ]
    );
}

#[test]
fn every_alternative_shadowed_by_its_own_shorter_sibling_is_dropped() {
    let (keymap, diagnostics) = parse("command \"X\" ctrl+a>b|ctrl+a|ctrl+a>c echo hi");
    assert_eq!(shortcuts(&keymap), ["ctrl+a"]);
    assert_eq!(
        messages(&diagnostics),
        [
            "custom command 'X' shortcut 'ctrl+a>b' conflicts with custom command 'X'; alternative dropped",
            "custom command 'X' shortcut 'ctrl+a>c' conflicts with custom command 'X'; alternative dropped",
        ]
    );
}

#[test]
fn alternative_order_inside_one_binding_does_not_decide_another_bindings_fate() {
    let long_first = parse("command \"A\" ctrl+a>b|ctrl+a echo a\ncommand \"B\" ctrl+a>c echo b");
    let short_first = parse("command \"A\" ctrl+a|ctrl+a>b echo a\ncommand \"B\" ctrl+a>c echo b");
    assert_eq!(shortcuts(&long_first.0), ["", ""]);
    assert_eq!(shortcuts(&short_first.0), shortcuts(&long_first.0));
    assert_eq!(sorted_messages(&short_first.1), sorted_messages(&long_first.1));
}

#[test]
fn sibling_order_does_not_decide_which_third_binding_survives() {
    let shortcuts_for = |alternatives: &str| {
        let text =
            format!("command \"A\" {alternatives} echo a\ncommand \"B\" ctrl+x echo b\ncommand \"C\" ctrl+x>2 echo c");
        let (keymap, _) = parse(&text);
        shortcuts(&keymap).into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    assert_eq!(shortcuts_for("ctrl+x>1|ctrl+x>2"), shortcuts_for("ctrl+x>2|ctrl+x>1"));
}

#[test]
fn a_bind_conflicting_with_two_others_drops_both_of_them_in_either_line_order() {
    let forward = parse("command \"A\" ctrl+a>b echo a\ncommand \"B\" ctrl+a echo b\ncommand \"C\" ctrl+a>c echo c");
    let reversed = parse("command \"C\" ctrl+a>c echo c\ncommand \"B\" ctrl+a echo b\ncommand \"A\" ctrl+a>b echo a");
    assert_eq!(shortcuts(&forward.0), ["", "", ""]);
    assert_eq!(shortcuts(&reversed.0), ["", "", ""]);
    assert_eq!(forward.1.len(), 3);
    assert_eq!(reversed.1.len(), 3);
}

#[test]
fn an_alternative_charged_for_a_conflict_with_a_dropped_one_goes_in_either_alternative_order() {
    let shortcuts_for = |alternatives: &str| {
        let (keymap, _) = parse(&format!(
            "command \"A\" {alternatives} echo a\ncommand \"B\" ctrl+a echo b"
        ));
        shortcuts(&keymap).into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    assert_eq!(shortcuts_for("ctrl+a|ctrl+a>b"), ["", ""]);
    assert_eq!(shortcuts_for("ctrl+a>b|ctrl+a"), ["", ""]);
}

#[test]
fn line_order_does_not_decide_which_bindings_survive() {
    let lines = [
        "map cmd+t|ctrl+a>t toggle_split",
        "command \"Boom\" ctrl+a>t|ctrl+b>t echo boom",
        "command \"Lead\" ctrl+c echo lead",
        "command \"Seq\" ctrl+c>s echo seq",
        "map cmd+y|ctrl+d>n new_session",
    ];
    let summary = |order: [usize; 5]| binding_summary(&parse(&order.map(|index| lines[index]).join("\n")).0);
    let forward = summary([0, 1, 2, 3, 4]);
    for expected in [
        "toggle_split=cmd+t",
        "new_session=cmd+y|ctrl+d>n",
        "Boom=ctrl+b>t",
        "Lead=",
        "Seq=",
    ] {
        assert!(
            forward.iter().any(|binding| binding == expected),
            "{expected} in {forward:?}"
        );
    }
    assert_eq!(summary([4, 3, 2, 1, 0]), forward);
    assert_eq!(summary([2, 4, 0, 3, 1]), forward);
}

#[test]
fn single_alternative_conflict_diagnostics_keep_todays_wording() {
    assert_eq!(
        messages(&parse("command \"Boom\" cmd+d echo boom").1),
        ["custom command 'Boom' shortcut 'cmd+d' conflicts with a built-in; keybind dropped"]
    );
    assert_eq!(
        messages(&parse("command \"Boom\" ctrl+1 echo boom").1),
        ["custom command 'Boom' shortcut 'ctrl+1' conflicts with a reserved shortcut; keybind dropped"]
    );
    assert_eq!(
        messages(&parse("command \"First\" cmd+shift+e echo one\ncommand \"Second\" cmd+shift+e echo two").1),
        [
            "custom command 'First' shortcut 'cmd+shift+e' conflicts with custom command 'Second'; keybind dropped",
            "custom command 'Second' shortcut 'cmd+shift+e' conflicts with custom command 'First'; keybind dropped",
        ]
    );
}

#[test]
fn a_reverted_overrides_revived_default_names_its_owner() {
    // new_session's cmd+d loses to toggle_split's default and reverts to cmd+n, which the command then hits:
    // the conflict was not visible as written, so the diagnostic names the owner.
    let (keymap, diagnostics) = parse("map cmd+d new_session\ncommand \"N\" cmd+n echo n");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(
        diagnostics,
        [
            diagnostic(1, "chord conflicts with built-in 'toggle_split'; map skipped"),
            diagnostic(
                0,
                "custom command 'N' shortcut 'cmd+n' conflicts with built-in 'new_session'; keybind dropped"
            ),
        ]
    );
}

/// The compatibility invariant: these are the pre-alternatives parser's outputs.
#[test]
fn a_pipe_free_keymap_parses_exactly_as_it_did_before_alternatives() {
    let (keymap, diagnostics) = parse(PIPE_FREE_KEYMAP_FIXTURE);
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([
            (A::ToggleSplit, chord(CMD | SHIFT, "e")),
            (A::ToggleSidebar, chord(NONE, "t")),
            (A::FocusLeftPane, chord(CTRL | CMD, "left")),
        ])
    );
    assert!(keymap.builtin_sequences().is_empty());
    assert!(keymap.builtin_unbound().is_empty());

    let names: Vec<&str> = keymap.commands().iter().map(|command| command.name.as_str()).collect();
    assert_eq!(names, ["Deploy", "Open Notes", "Clash", "First", "Second"]);
    assert_eq!(shortcuts(&keymap), ["cmd+shift+y", "", "", "", ""]);
    let bodies: Vec<&str> = keymap
        .commands()
        .iter()
        .map(|command| command.command.as_str())
        .collect();
    assert_eq!(
        bodies,
        [
            "./deploy.sh",
            "vim {AGT_SESSION_PWD}/notes.md",
            "echo clash",
            "echo one",
            "echo two"
        ]
    );

    assert_eq!(
        diagnostics,
        [
            diagnostic(5, "chord conflicts with built-in 'close_session'; map skipped"),
            diagnostic(
                0,
                "custom command 'Clash' shortcut 'command+shift+e' conflicts with a built-in; keybind dropped"
            ),
            diagnostic(
                0,
                "custom command 'First' shortcut 'control+shift+g' conflicts with custom command 'Second'; keybind dropped"
            ),
            diagnostic(
                0,
                "custom command 'Second' shortcut 'ctrl+shift+g' conflicts with custom command 'First'; keybind dropped"
            ),
        ]
    );
}

#[test]
fn the_conformance_keymap_reports_one_diagnostic_for_its_one_bad_line() {
    let text = "command \"mark\" ctrl+shift+f11 --error-hud printf '%s' \"$AGT_SESSION_ID\" > /x\n\
                map ctrl+shift+f12 new_session\n\
                bogus line\n";
    let (keymap, diagnostics) = parse(text);
    assert_eq!(diagnostics, [diagnostic(3, "unknown verb 'bogus'")]);
    let command = &keymap.commands()[0];
    assert_eq!(command.shortcut, "ctrl+shift+f11");
    assert!(command.error_hud);
    assert_eq!(command.command, "printf '%s' \"$AGT_SESSION_ID\" > /x");
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CTRL | SHIFT, "f12")));
}
