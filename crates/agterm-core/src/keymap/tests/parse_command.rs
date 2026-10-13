//! Ported from upstream's `KeymapTests`: the `command` verb, its shortcut token and its options.

use super::*;
use crate::hud::HudPosition;
use crate::model::OverlayPane;

#[test]
fn parse_command_happy_path() {
    let (keymap, diagnostics) = parse("command \"Lazygit\" ctrl+a>g lazygit");
    assert!(diagnostics.is_empty());
    assert!(keymap.builtin_overrides().is_empty());
    let command = &keymap.commands()[0];
    assert_eq!(
        (command.name.as_str(), command.shortcut.as_str()),
        ("Lazygit", "ctrl+a>g")
    );
    assert_eq!(command.command, "lazygit");
}

#[test]
fn a_quoted_name_may_hold_spaces() {
    let (keymap, diagnostics) = parse("command \"Open in Zed\" cmd+shift+e open -a Zed {AGT_SESSION_PWD}");
    assert!(diagnostics.is_empty());
    let command = &keymap.commands()[0];
    assert_eq!(
        (command.name.as_str(), command.shortcut.as_str()),
        ("Open in Zed", "cmd+shift+e")
    );
    assert_eq!(command.command, "open -a Zed {AGT_SESSION_PWD}");
}

#[test]
fn a_command_without_a_chord_is_palette_only() {
    let (keymap, diagnostics) = parse("command \"Deploy\" ./deploy.sh");
    assert!(diagnostics.is_empty());
    let command = &keymap.commands()[0];
    assert_eq!((command.name.as_str(), command.shortcut.as_str()), ("Deploy", ""));
    assert_eq!(command.command, "./deploy.sh");
}

#[test]
fn a_duplicate_name_keeps_the_first_definition() {
    let (keymap, diagnostics) = parse(
        "command \"Deploy\" cmd+shift+d ./deploy.sh\ncommand \"Deploy\" ./deploy.sh --prod\ncommand \"Other\" ./other.sh",
    );
    let names: Vec<&str> = keymap.commands().iter().map(|command| command.name.as_str()).collect();
    assert_eq!(names, ["Deploy", "Other"]);
    assert_eq!(keymap.commands()[0].command, "./deploy.sh");
    assert_eq!(keymap.commands()[0].shortcut, "cmd+shift+d");
    assert_eq!(
        diagnostics,
        [diagnostic(2, "command 'Deploy' is already defined; command skipped")]
    );
}

#[test]
fn a_bare_key_is_never_taken_as_a_shortcut() {
    let (keymap, diagnostics) = parse("command \"X\" a echo hi");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(keymap.commands()[0].command, "a echo hi");
    assert_eq!(
        messages(&diagnostics),
        ["command 'X' shortcut 'a' must include a modifier; treating the line as palette-only"]
    );
}

#[test]
fn a_modifier_shortcut_is_accepted() {
    let (keymap, diagnostics) = parse("command \"X\" cmd+e echo hi");
    assert!(diagnostics.is_empty());
    assert_eq!(shortcuts(&keymap), ["cmd+e"]);
    assert_eq!(keymap.commands()[0].command, "echo hi");
}

#[test]
fn a_leading_single_character_shell_token_is_not_swallowed() {
    let (keymap, diagnostics) = parse("command \"Check\" [ -f /tmp ] && echo ok");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(keymap.commands()[0].command, "[ -f /tmp ] && echo ok");
    assert_eq!(diagnostics.len(), 1);
}

#[test]
fn alternative_shortcuts_are_kept_verbatim() {
    let (keymap, diagnostics) = parse("command \"Midnight Commander\" ctrl+a>m|cmd+a>m mc");
    assert!(diagnostics.is_empty());
    assert_eq!(shortcuts(&keymap), ["ctrl+a>m|cmd+a>m"]);
    assert_eq!(keymap.commands()[0].command, "mc");
}

#[test]
fn identical_alternatives_are_deduped() {
    let (keymap, diagnostics) = parse("command \"X\" cmd+e|command+e echo hi");
    assert!(diagnostics.is_empty());
    assert_eq!(shortcuts(&keymap), ["cmd+e"]);
    assert_eq!(keymap.commands()[0].command, "echo hi");
}

#[test]
fn a_bare_alternative_drops_alone_and_keeps_the_sibling() {
    let (keymap, diagnostics) = parse("command \"X\" cmd+e|t echo hi");
    assert_eq!(shortcuts(&keymap), ["cmd+e"]);
    assert_eq!(keymap.commands()[0].command, "echo hi");
    assert_eq!(
        messages(&diagnostics),
        ["command 'X' shortcut 't' must include a modifier; alternative skipped"]
    );
}

#[test]
fn no_modifier_on_any_alternative_stays_palette_only() {
    let (keymap, diagnostics) = parse("command \"X\" t|y echo hi");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(keymap.commands()[0].command, "t|y echo hi");
    assert_eq!(
        messages(&diagnostics),
        ["command 'X' shortcut 't|y' must include a modifier; treating the line as palette-only"]
    );
}

#[test]
fn a_malformed_alternative_is_diagnosed_and_leaves_the_line_palette_only() {
    let (keymap, diagnostics) = parse("command \"X\" cmd+e|f21 echo hi");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(keymap.commands()[0].command, "cmd+e|f21 echo hi");
    assert_eq!(
        messages(&diagnostics),
        ["command 'X' shortcut 'cmd+e|f21' has an invalid alternative; treating the line as palette-only"]
    );
}

#[test]
fn alternatives_conflicting_with_each_other_keep_the_firing_one() {
    let (keymap, diagnostics) = parse("command \"X\" ctrl+a|ctrl+a>b echo hi");
    assert_eq!(shortcuts(&keymap), ["ctrl+a"]);
    assert_eq!(
        messages(&diagnostics),
        ["custom command 'X' shortcut 'ctrl+a>b' conflicts with custom command 'X'; alternative dropped"]
    );
}

#[test]
fn a_leading_shell_pipeline_is_not_diagnosed_as_a_binding() {
    let (keymap, diagnostics) = parse("command \"X\" ls|grep foo");
    assert!(diagnostics.is_empty());
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(keymap.commands()[0].command, "ls|grep foo");
}

#[test]
fn a_piped_shell_token_keeps_its_shell_line() {
    let (keymap, diagnostics) = parse("command \"X\" a|b echo hi");
    assert_eq!(shortcuts(&keymap), [""]);
    assert_eq!(keymap.commands()[0].command, "a|b echo hi");
    assert_eq!(
        messages(&diagnostics),
        ["command 'X' shortcut 'a|b' must include a modifier; treating the line as palette-only"]
    );
}

#[test]
fn agt_tokens_are_preserved() {
    let (keymap, diagnostics) = parse("command \"Notify\" echo {AGT_SELECTION} > {AGT_SOCKET}");
    assert!(diagnostics.is_empty());
    assert_eq!(keymap.commands()[0].command, "echo {AGT_SELECTION} > {AGT_SOCKET}");
}

#[test]
fn a_command_without_a_quoted_name_is_diagnosed() {
    for text in [
        "command Deploy ./deploy.sh",
        "command \"Unclosed ./deploy.sh",
        "command",
    ] {
        let (keymap, diagnostics) = parse(text);
        assert!(keymap.commands().is_empty());
        assert_eq!(messages(&diagnostics), ["command requires a quoted name"], "{text}");
    }
}

#[test]
fn a_command_with_an_empty_shell_line_is_diagnosed() {
    for text in ["command \"Empty\"", "command \"Empty\" cmd+shift+e"] {
        let (keymap, diagnostics) = parse(text);
        assert!(keymap.commands().is_empty());
        assert_eq!(
            diagnostics,
            [diagnostic(1, "command 'Empty' has no shell line")],
            "{text}"
        );
    }
}

#[test]
fn error_options_parse_in_any_order_after_the_chord() {
    let (keymap, diagnostics) =
        parse("command \"Deploy\" cmd+shift+y --error-pane right --error-hud --error-position top ./deploy.sh --prod");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let command = &keymap.commands()[0];
    assert!(command.error_hud);
    assert_eq!(command.error_position, HudPosition::TopCenter, "aliases are accepted");
    assert_eq!(command.error_pane, Some(OverlayPane::Right));
    assert_eq!(command.command, "./deploy.sh --prod");
    assert_eq!(command.shortcut, "cmd+shift+y");
}

#[test]
fn options_without_a_chord_and_after_repeat() {
    let (keymap, diagnostics) = parse("command \"Grow\" ctrl+a>x --repeat --error-hud -- --error-hud is shell");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let command = &keymap.commands()[0];
    assert!(command.repeats && command.error_hud);
    assert_eq!(command.error_position, HudPosition::Center);
    assert_eq!(command.error_pane, None);
    assert_eq!(command.command, "--error-hud is shell");

    let (keymap, diagnostics) = parse("command \"Plain\" --error-hud echo hi");
    assert!(diagnostics.is_empty());
    assert!(keymap.commands()[0].error_hud);
    assert_eq!(keymap.commands()[0].command, "echo hi");
}

#[test]
fn the_first_ordinary_word_ends_the_options() {
    let (keymap, diagnostics) = parse("command \"X\" echo --error-hud --repeat");
    assert!(diagnostics.is_empty());
    let command = &keymap.commands()[0];
    assert!(!command.error_hud && !command.repeats);
    assert_eq!(command.command, "echo --error-hud --repeat");
}

#[test]
fn every_option_rejection_skips_the_command() {
    for (line, reason) in [
        ("--error-bogus echo hi", "has unknown option '--error-bogus'"),
        ("--error-hud --error-hud echo hi", "repeats option '--error-hud'"),
        ("--repeat --repeat echo hi", "repeats option '--repeat'"),
        (
            "--error-hud --error-position nowhere echo hi",
            "requires a valid HUD position after --error-position",
        ),
        (
            "--error-hud --error-position",
            "requires a valid HUD position after --error-position",
        ),
        (
            "--error-hud --error-pane top echo hi",
            "requires left or right after --error-pane",
        ),
        (
            "--error-hud --error-pane scratch echo hi",
            "requires left or right after --error-pane",
        ),
        (
            "--error-pane left echo hi",
            "requires --error-hud for error placement options",
        ),
        (
            "--error-position top-left echo hi",
            "requires --error-hud for error placement options",
        ),
        ("--error-hud --", "has no shell line"),
        ("--repeat", "has no shell line"),
    ] {
        let (keymap, diagnostics) = parse(&format!("command \"X\" cmd+e {line}"));
        assert!(keymap.commands().is_empty(), "{line}");
        let expected = format!("command 'X' {reason}");
        assert_eq!(diagnostics, [diagnostic(1, &expected)], "{line}");
    }
}

#[test]
fn the_error_pane_takes_exactly_left_or_right() {
    for (value, pane) in [("left", OverlayPane::Left), ("right", OverlayPane::Right)] {
        let (keymap, diagnostics) = parse(&format!("command \"X\" --error-hud --error-pane {value} echo hi"));
        assert!(diagnostics.is_empty());
        assert_eq!(keymap.commands()[0].error_pane, Some(pane));
    }
    for alias in ["primary", "split", "bottom"] {
        let (keymap, _) = parse(&format!("command \"X\" --error-hud --error-pane {alias} echo hi"));
        assert!(
            keymap.commands().is_empty(),
            "{alias} is a control alias, not an --error-pane value"
        );
    }
}

#[test]
fn repeat_survives_only_beside_a_surviving_leader_sequence() {
    let (keymap, diagnostics) = parse("command \"Seq\" ctrl+a>x --repeat ./s\ncommand \"Single\" cmd+g --repeat ./s");
    assert!(diagnostics.is_empty());
    let repeats: Vec<bool> = keymap.commands().iter().map(|command| command.repeats).collect();
    assert_eq!(repeats, [true, false]);
}
