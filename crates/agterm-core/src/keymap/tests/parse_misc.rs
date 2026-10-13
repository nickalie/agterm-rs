//! Ported from upstream's `KeymapTests`: function keys, menu chord resolution, `global-hotkey`, comments, line
//! handling and the compatibility vacating of new shipped defaults.

use super::*;

use BuiltinAction as A;

#[test]
fn function_key_commands() {
    for shortcut in ["f5", "f20", "f5>x", "ctrl+f5>x", "shift+f6"] {
        let (keymap, diagnostics) = parse(&format!("command \"Run\" {shortcut} echo hello"));
        assert!(diagnostics.is_empty(), "{shortcut}: {diagnostics:?}");
        assert_eq!(keymap.commands()[0].shortcut, shortcut);
        assert_eq!(keymap.commands()[0].command, "echo hello");
    }
}

#[test]
fn a_bare_function_key_map_and_global_hotkey() {
    for key in ["f5", "f20"] {
        let (keymap, diagnostics) = parse(&format!("map {key} toggle_sidebar\nglobal-hotkey {key}"));
        assert!(diagnostics.is_empty(), "{key}: {diagnostics:?}");
        assert_eq!(keymap.equivalent(A::ToggleSidebar), Some(&chord(NONE, key)));
        assert_eq!(keymap.global_hotkey(), Some(&chord(NONE, key)));
    }
}

#[test]
fn bare_function_key_map_monitor_bindings() {
    for shortcut in ["cmd+s|f5", "cmd+s|f5>x", "f5>x"] {
        let (keymap, diagnostics) = parse(&format!("map {shortcut} toggle_sidebar"));
        assert!(diagnostics.is_empty(), "{shortcut}: {diagnostics:?}");
        let tail = shortcut.rsplit('|').next().unwrap_or_default();
        assert_eq!(
            keymap.builtin_sequences().get(&A::ToggleSidebar),
            Some(&vec![keybind(tail)]),
            "{shortcut}"
        );
    }
}

#[test]
fn ordinary_bare_first_chords_are_still_rejected() {
    for key in ["x", "left", "space"] {
        let (commands, command_diagnostics) = parse(&format!("command \"Run\" {key}>x echo hello"));
        assert_eq!(commands.commands()[0].shortcut, "");
        assert!(!command_diagnostics.is_empty());
        let (maps, map_diagnostics) = parse(&format!("map {key}>x toggle_sidebar"));
        assert!(maps.builtin_sequences().is_empty());
        assert!(!map_diagnostics.is_empty());
        let (global, global_diagnostics) = parse(&format!("global-hotkey {key}"));
        assert_eq!(global.global_hotkey(), None);
        assert!(!global_diagnostics.is_empty());
    }
}

#[test]
fn an_override_wins_over_the_default() {
    let override_chord = chord(CMD | SHIFT, "e");
    let keymap = keymap_with_overrides([(A::ToggleSplit, override_chord.clone())]);
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&override_chord));
    assert_ne!(default_chord(A::ToggleSplit), Some(override_chord));
}

#[test]
fn an_absent_override_falls_back_to_the_default() {
    let keymap = keymap_with_overrides([]);
    assert_eq!(
        keymap.equivalent(A::ToggleSplit).cloned(),
        default_chord(A::ToggleSplit)
    );
    assert_eq!(keymap.equivalent(A::NewSession), Some(&chord(CMD, "n")));
}

#[test]
fn a_keyless_action_resolves_to_its_override_or_nothing() {
    assert_eq!(default_chord(A::FirstSession), None);
    let override_chord = chord(CMD, "1");
    let keymap = keymap_with_overrides([(A::FirstSession, override_chord.clone())]);
    assert_eq!(keymap.equivalent(A::FirstSession), Some(&override_chord));
    let empty = keymap_with_overrides([]);
    assert_eq!(empty.equivalent(A::FirstSession), None);
    assert_eq!(empty.equivalent(A::FocusLeftPane), Some(&chord(CMD | OPT, "left")));
}

#[test]
fn parse_map_happy_path() {
    let (keymap, diagnostics) = parse("map cmd+shift+e toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert!(keymap.commands().is_empty());
}

#[test]
fn parse_global_hotkey_happy_path() {
    let (keymap, diagnostics) = parse("global-hotkey ctrl+opt+space");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.global_hotkey(), Some(&chord(CTRL | OPT, "space")));
    assert!(keymap.builtin_overrides().is_empty());
    assert!(keymap.commands().is_empty());
}

#[test]
fn an_absent_global_hotkey_is_none() {
    let (keymap, diagnostics) = parse("map cmd+shift+e toggle_split");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.global_hotkey(), None);
}

#[test]
fn the_last_global_hotkey_line_wins() {
    let (keymap, diagnostics) = parse("global-hotkey ctrl+opt+space\nglobal-hotkey cmd+shift+j");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.global_hotkey(), Some(&chord(CMD | SHIFT, "j")));
}

#[test]
fn a_bare_global_hotkey_chord_is_rejected() {
    let (keymap, diagnostics) = parse("global-hotkey j");
    assert_eq!(keymap.global_hotkey(), None);
    assert_eq!(
        diagnostics,
        [diagnostic(1, "global-hotkey 'j' must include a modifier")]
    );
}

#[test]
fn a_global_hotkey_sequence_is_rejected() {
    let (keymap, diagnostics) = parse("global-hotkey ctrl+a>j");
    assert_eq!(keymap.global_hotkey(), None);
    assert_eq!(diagnostics, [diagnostic(1, "invalid global-hotkey chord 'ctrl+a>j'")]);
}

#[test]
fn a_global_hotkey_with_no_key_position_is_rejected() {
    for token in ["ctrl+opt+é", "ctrl+opt+!", "cmd+ю"] {
        let (keymap, diagnostics) = parse(&format!("global-hotkey {token}"));
        assert_eq!(keymap.global_hotkey(), None, "{token} should not bind");
        let expected = format!("global-hotkey '{token}' names no key position; write a shifted symbol as shift+<base>");
        assert_eq!(messages(&diagnostics), [expected.as_str()]);
    }
}

#[test]
fn a_global_hotkey_accepts_every_bindable_named_key() {
    for key in bindable_named_keys() {
        let (keymap, diagnostics) = parse(&format!("global-hotkey ctrl+opt+{key}"));
        assert!(diagnostics.is_empty(), "{key} should parse clean");
        assert_eq!(keymap.global_hotkey(), Some(&chord(CTRL | OPT, key)));
    }
}

#[test]
fn a_global_hotkey_rejects_missing_and_extra_tokens() {
    let (missing, missing_diagnostics) = parse("global-hotkey");
    assert_eq!(missing.global_hotkey(), None);
    assert_eq!(messages(&missing_diagnostics), ["global-hotkey needs a chord"]);

    let (extra, extra_diagnostics) = parse("global-hotkey ctrl+opt+space quick_terminal");
    assert_eq!(extra.global_hotkey(), None);
    assert_eq!(
        messages(&extra_diagnostics),
        ["global-hotkey takes one chord, found 'ctrl+opt+space quick_terminal'"]
    );
}

#[test]
fn a_global_hotkey_does_not_collide_with_a_builtin_menu_chord() {
    let (keymap, diagnostics) = parse("map cmd+shift+j toggle_split\nglobal-hotkey cmd+shift+j");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.global_hotkey(), Some(&chord(CMD | SHIFT, "j")));
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "j"))])
    );
}

#[test]
fn a_bad_global_hotkey_line_does_not_discard_the_rest_of_the_file() {
    let (keymap, diagnostics) = parse("global-hotkey j\nmap cmd+shift+e toggle_split");
    assert_eq!(keymap.global_hotkey(), None);
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line, 1);
}

#[test]
fn comments_and_blank_lines_are_skipped() {
    let text = "# leading comment\n\nmap cmd+shift+e toggle_split  # inline comment\n# another comment\n\
                command \"Deploy\" ./deploy.sh";
    let (keymap, diagnostics) = parse(text);
    assert!(diagnostics.is_empty());
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert_eq!(keymap.commands().len(), 1);
    assert_eq!(keymap.commands()[0].name, "Deploy");
}

#[test]
fn an_inline_comment_inside_a_quoted_name_is_kept() {
    let (keymap, diagnostics) = parse("command \"name # not a comment\" echo hi");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.commands()[0].name, "name # not a comment");
}

#[test]
fn a_hash_inside_a_single_quoted_shell_argument_is_kept() {
    let (keymap, diagnostics) = parse("command \"Commit\" cmd+shift+c git commit -m 'fix #42'");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.commands()[0].command, "git commit -m 'fix #42'");
}

#[test]
fn a_trailing_comment_after_a_single_quoted_argument_is_stripped() {
    let (keymap, diagnostics) = parse("command \"X\" cmd+shift+x echo 'hi' # real comment");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.commands()[0].command, "echo 'hi'");
}

#[test]
fn a_double_quote_inside_single_quotes_does_not_open_a_quoted_span() {
    let (keymap, diagnostics) = parse("command \"Y\" cmd+shift+y echo 'a\"b' # c");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.commands()[0].command, "echo 'a\"b'");
}

#[test]
fn a_hash_glued_to_a_word_is_not_a_comment() {
    let (keymap, diagnostics) = parse("command \"Issue\" echo issue#42");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.commands()[0].command, "echo issue#42");
}

#[test]
fn an_unknown_verb_is_diagnosed() {
    let (keymap, diagnostics) = parse("bind cmd+d toggle_split");
    assert!(keymap.builtin_overrides().is_empty());
    assert_eq!(diagnostics, [diagnostic(1, "unknown verb 'bind'")]);
}

#[test]
fn crlf_line_endings_parse() {
    let (keymap, diagnostics) = parse("map cmd+shift+e toggle_split\r\ncommand \"Deploy\" ./deploy.sh\r\n");
    assert!(diagnostics.is_empty());
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert_eq!(keymap.commands()[0].name, "Deploy");
}

#[test]
fn lone_cr_line_endings_split_lines() {
    let (keymap, diagnostics) = parse("bogus\rmap cmd+shift+e toggle_split");
    assert_eq!(diagnostics, [diagnostic(1, "unknown verb 'bogus'")]);
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD | SHIFT, "e")));
}

#[test]
fn diagnostic_line_numbers_track_the_file_while_good_lines_parse() {
    let text = "map cmd+shift+e toggle_split\nbogus line here\ncommand \"Deploy\" ./deploy.sh\nmap cmd+f21 new_session";
    let (keymap, diagnostics) = parse(text);
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleSplit, chord(CMD | SHIFT, "e"))])
    );
    assert_eq!(keymap.commands()[0].name, "Deploy");
    assert_eq!(
        diagnostics,
        [
            diagnostic(2, "unknown verb 'bogus'"),
            diagnostic(4, "invalid chord 'cmd+f21'")
        ]
    );
}

#[test]
fn an_empty_keymap_leaves_every_builtin_on_its_shipped_default() {
    let (keymap, diagnostics) = parse("");
    assert!(diagnostics.is_empty());
    for action in BuiltinAction::all() {
        assert_eq!(keymap.equivalent(action).cloned(), default_chord(action));
        assert!(keymap.sequences(action).is_empty());
    }
    assert_eq!(keymap, Keymap::empty(&ShippedDefaults::upstream()));
}

#[test]
fn split_and_dashboard_ship_their_distinct_defaults() {
    let (keymap, _) = parse("");
    assert_eq!(keymap.equivalent(A::ToggleSplit), Some(&chord(CMD, "d")));
    assert_eq!(
        keymap.equivalent(A::ToggleHorizontalSplit),
        Some(&chord(CMD | SHIFT, "d"))
    );
    assert_eq!(keymap.equivalent(A::Dashboard), Some(&chord(CMD | SHIFT, "g")));
}

#[test]
fn a_legacy_explicit_dashboard_binding_keeps_cmd_shift_d() {
    let (keymap, diagnostics) = parse("map cmd+shift+d dashboard");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(keymap.equivalent(A::Dashboard), Some(&chord(CMD | SHIFT, "d")));
    assert_eq!(keymap.equivalent(A::ToggleHorizontalSplit), None);
    assert!(keymap.builtin_unbound().contains(&A::ToggleHorizontalSplit));
}

#[test]
fn an_existing_cmd_shift_d_command_is_not_broken_by_the_horizontal_split_default() {
    let (keymap, diagnostics) = parse("map ctrl+shift+y dashboard\ncommand \"Old binding\" cmd+shift+d echo ok");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(shortcuts(&keymap), ["cmd+shift+d"]);
    assert_eq!(keymap.equivalent(A::ToggleHorizontalSplit), None);
    assert!(keymap.builtin_unbound().contains(&A::ToggleHorizontalSplit));
}

#[test]
fn an_existing_cmd_shift_g_builtin_binding_is_not_broken_by_the_dashboard_default() {
    let (keymap, diagnostics) = parse("map cmd+shift+g toggle_workspace_filter");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        keymap.equivalent(A::ToggleWorkspaceFilter),
        Some(&chord(CMD | SHIFT, "g"))
    );
    assert_eq!(keymap.equivalent(A::Dashboard), None);
    assert!(keymap.builtin_unbound().contains(&A::Dashboard));
}

#[test]
fn an_existing_cmd_shift_g_command_is_not_broken_by_the_dashboard_default() {
    let (keymap, diagnostics) = parse("command \"Old binding\" cmd+shift+g echo ok");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(shortcuts(&keymap), ["cmd+shift+g"]);
    assert_eq!(keymap.equivalent(A::Dashboard), None);
}

#[test]
fn attach_remote_ships_on_cmd_shift_r_and_can_be_remapped() {
    assert_eq!(parse("").0.equivalent(A::AttachRemote), Some(&chord(CMD | SHIFT, "r")));
    let (keymap, diagnostics) = parse("map ctrl+shift+y attach_remote");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.equivalent(A::AttachRemote), Some(&chord(CTRL | SHIFT, "y")));
}

#[test]
fn an_existing_cmd_shift_r_builtin_binding_is_not_broken_by_the_attach_remote_default() {
    let (keymap, diagnostics) = parse("map cmd+shift+r toggle_workspace_filter");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        keymap.equivalent(A::ToggleWorkspaceFilter),
        Some(&chord(CMD | SHIFT, "r"))
    );
    assert_eq!(keymap.equivalent(A::AttachRemote), None);
    assert!(keymap.builtin_unbound().contains(&A::AttachRemote));
}

#[test]
fn an_existing_cmd_shift_r_command_is_not_broken_by_the_attach_remote_default() {
    let (keymap, diagnostics) = parse("command \"Old binding\" cmd+shift+r echo ok");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(shortcuts(&keymap), ["cmd+shift+r"]);
    assert_eq!(keymap.equivalent(A::AttachRemote), None);
}

#[test]
fn an_explicit_attach_remote_map_keeps_the_chord_over_an_old_command_on_it() {
    let (keymap, _) = parse("map cmd+shift+r attach_remote\ncommand \"Old binding\" cmd+shift+r echo ok");
    assert_eq!(keymap.equivalent(A::AttachRemote), Some(&chord(CMD | SHIFT, "r")));
    assert_eq!(shortcuts(&keymap), [""]);
}

#[test]
fn a_vacated_default_is_also_freed_by_another_maps_monitor_alternative() {
    let (keymap, diagnostics) = parse("map cmd+t|cmd+shift+g>x toggle_split");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(keymap.equivalent(A::Dashboard), None);
    assert_eq!(keymap.sequences(A::ToggleSplit), [keybind("cmd+shift+g>x")]);
}

#[test]
fn unbinding_a_vacatable_action_explicitly_keeps_its_default_free_for_others() {
    // dashboard is mapped (to a sequence only), so it is not vacated for compatibility: the file opted in.
    let (keymap, diagnostics) = parse("map ctrl+a>g dashboard\ncommand \"G\" cmd+shift+g echo g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(keymap.equivalent(A::Dashboard), None);
    assert_eq!(shortcuts(&keymap), ["cmd+shift+g"]);
}
