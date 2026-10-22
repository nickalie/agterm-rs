//! Ported from upstream's `BuiltinActionTests`, against [`ShippedDefaults::upstream`] (the glyph halves stay with
//! the frontend).

use std::collections::HashSet;

use super::*;

use BuiltinAction as A;

#[test]
fn every_name_round_trips_and_unknown_names_are_rejected() {
    for action in BuiltinAction::all() {
        assert_eq!(BuiltinAction::from_name(action.name()), Some(action));
    }
    for name in ["not_an_action", "", "New_Window"] {
        assert_eq!(BuiltinAction::from_name(name), None, "{name:?}");
    }
}

#[test]
fn names_are_the_kitty_style_names() {
    for (action, name) in [
        (A::NewWindow, "new_window"),
        (A::ToggleSplit, "toggle_split"),
        (A::ToggleHorizontalSplit, "toggle_horizontal_split"),
        (A::ToggleTerminalZoom, "toggle_terminal_zoom"),
        (A::ToggleSearch, "toggle_search"),
        (A::CommandPalette, "command_palette"),
        (A::CustomCommandPalette, "custom_command_palette"),
        (A::NextAttentionSession, "next_attention_session"),
        (A::ToggleSidebar, "toggle_sidebar"),
        (A::SelectTheme, "select_theme"),
        (A::ToggleFlaggedView, "toggle_flagged_view"),
        (A::ToggleWorkspaceFilter, "toggle_workspace_filter"),
        (A::ToggleFlag, "toggle_flag"),
        (A::FocusWorkspace, "focus_workspace"),
        (A::ShowAttention, "show_attention"),
        (A::ReopenRecent, "reopen_recent"),
        (A::UndoClose, "undo_close"),
        (A::ToggleFullscreen, "toggle_fullscreen"),
        (A::Dashboard, "dashboard"),
        (A::DuplicateSession, "duplicate_session"),
        (A::PreviousWorkspace, "previous_workspace"),
        (A::NextWorkspace, "next_workspace"),
        (A::ToggleWorkspaceCollapse, "toggle_workspace_collapse"),
        (A::PreviousWindow, "previous_window"),
        (A::NextWindow, "next_window"),
        (A::AttachRemote, "attach_remote"),
    ] {
        assert_eq!(action.name(), name);
    }
    assert_eq!(BuiltinAction::all().count(), 49);
}

#[test]
fn arrow_bound_actions_ship_real_defaults_that_round_trip() {
    for (action, syntax) in [
        (A::FocusLeftPane, "cmd+opt+left"),
        (A::FocusRightPane, "cmd+opt+right"),
        (A::PreviousSession, "cmd+opt+up"),
        (A::NextSession, "cmd+opt+down"),
        (A::PreviousAttentionSession, "ctrl+opt+up"),
        (A::NextAttentionSession, "ctrl+opt+down"),
    ] {
        let default = default_chord(action);
        assert_eq!(
            default.as_ref().map(Chord::display_string).as_deref(),
            Some(syntax),
            "{}",
            action.name()
        );
        assert_eq!(parse_keybind(syntax), default.map(|chord| vec![chord]));
    }
}

#[test]
fn every_shipped_default_round_trips_except_the_documented_plus_key() {
    let not_expressible: Vec<BuiltinAction> = BuiltinAction::all()
        .filter(|&action| {
            default_chord(action).is_some_and(|chord| parse_keybind(&chord.display_string()) != Some(vec![chord]))
        })
        .collect();
    assert_eq!(not_expressible, [A::IncreaseFontSize]);
}

#[test]
fn shipped_defaults_are_all_distinct() {
    let defaults: Vec<Chord> = BuiltinAction::all().filter_map(default_chord).collect();
    assert_eq!(defaults.iter().collect::<HashSet<_>>().len(), defaults.len());
}

#[test]
fn the_default_chord_matches_the_shipped_table() {
    let expected: Vec<(BuiltinAction, Option<Chord>)> = vec![
        (A::NewWindow, Some(chord(CMD | OPT, "n"))),
        (A::RenameWindow, None),
        (A::DeleteWindow, None),
        (A::PreviousWindow, None),
        (A::NextWindow, None),
        (A::NewWorkspace, Some(chord(CMD | SHIFT, "n"))),
        (A::RenameWorkspace, None),
        (A::DeleteWorkspace, None),
        (A::NewSession, Some(chord(CMD, "n"))),
        (A::OpenDirectory, Some(chord(CMD, "o"))),
        (A::RenameSession, None),
        (A::DuplicateSession, None),
        (A::CloseSession, Some(chord(CMD, "w"))),
        (A::ReopenRecent, Some(chord(CMD | SHIFT, "t"))),
        (A::UndoClose, Some(chord(CMD, "z"))),
        (A::ClearStatus, None),
        (A::IncreaseFontSize, Some(chord(CMD, "+"))),
        (A::DecreaseFontSize, Some(chord(CMD, "-"))),
        (A::ResetFontSize, Some(chord(CMD, "0"))),
        (A::ToggleSplit, Some(chord(CMD, "d"))),
        (A::ToggleHorizontalSplit, Some(chord(CMD | SHIFT, "d"))),
        (A::ToggleScratch, Some(chord(CMD, "j"))),
        (A::ToggleTerminalZoom, Some(chord(CMD | SHIFT, "return"))),
        (A::ToggleSearch, Some(chord(CMD, "f"))),
        (A::ToggleSidebar, Some(chord(CMD | CTRL, "s"))),
        (A::SelectTheme, None),
        (A::ToggleFullscreen, Some(chord(CMD | CTRL, "f"))),
        (A::ToggleFlaggedView, None),
        (A::ToggleFlag, Some(chord(CMD | SHIFT, "f"))),
        (A::FocusWorkspace, None),
        (A::ToggleWorkspaceFilter, None),
        (A::PreviousWorkspace, None),
        (A::NextWorkspace, None),
        (A::ToggleWorkspaceCollapse, None),
        (A::FocusLeftPane, Some(chord(CMD | OPT, "left"))),
        (A::FocusRightPane, Some(chord(CMD | OPT, "right"))),
        (A::PreviousSession, Some(chord(CMD | OPT, "up"))),
        (A::NextSession, Some(chord(CMD | OPT, "down"))),
        (A::PreviousAttentionSession, Some(chord(CTRL | OPT, "up"))),
        (A::NextAttentionSession, Some(chord(CTRL | OPT, "down"))),
        (A::FirstSession, None),
        (A::LastSession, None),
        (A::QuickTerminal, Some(chord(CTRL, "`"))),
        (A::SessionPalette, Some(chord(CTRL, "p"))),
        (A::CommandPalette, Some(chord(CTRL | SHIFT, "p"))),
        (A::CustomCommandPalette, Some(chord(CTRL | SHIFT, "o"))),
        (A::ShowAttention, Some(chord(CTRL | SHIFT, "i"))),
        (A::Dashboard, Some(chord(CMD | SHIFT, "g"))),
        (A::AttachRemote, Some(chord(CMD | SHIFT, "r"))),
    ];
    assert_eq!(expected.len(), 49);
    for (action, chord) in expected {
        assert_eq!(default_chord(action), chord, "{}", action.name());
    }
    assert_eq!(
        ShippedDefaults::upstream().vacate_when_used(),
        [A::ToggleHorizontalSplit, A::Dashboard, A::AttachRemote]
    );
}

#[test]
fn single_defaults_round_trip() {
    for (action, syntax) in [
        (A::ToggleSearch, "cmd+f"),
        (A::ToggleSidebar, "ctrl+cmd+s"),
        (A::ToggleFullscreen, "ctrl+cmd+f"),
        (A::CustomCommandPalette, "ctrl+shift+o"),
        (A::ToggleFlag, "cmd+shift+f"),
    ] {
        let default = default_chord(action);
        assert_eq!(default.as_ref().map(Chord::display_string).as_deref(), Some(syntax));
        assert_eq!(parse_keybind(syntax), default.map(|chord| vec![chord]));
    }
}

#[test]
fn toggle_workspace_filter_is_keyless_and_mappable() {
    assert_eq!(default_chord(A::ToggleWorkspaceFilter), None);
    let (keymap, diagnostics) = parse("map cmd+shift+g toggle_workspace_filter");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let override_chord = chord(CMD | SHIFT, "g");
    assert_eq!(
        *keymap.builtin_overrides(),
        overrides([(A::ToggleWorkspaceFilter, override_chord.clone())])
    );
    assert_eq!(keymap.equivalent(A::ToggleWorkspaceFilter), Some(&override_chord));
    assert_eq!(
        Keymap::empty(&ShippedDefaults::upstream()).equivalent(A::ToggleWorkspaceFilter),
        None
    );
}

#[test]
fn keyless_actions_have_no_default() {
    let keyless = [
        A::RenameWindow,
        A::DeleteWindow,
        A::RenameWorkspace,
        A::DeleteWorkspace,
        A::RenameSession,
        A::DuplicateSession,
        A::ClearStatus,
        A::FirstSession,
        A::LastSession,
        A::SelectTheme,
        A::ToggleFlaggedView,
        A::FocusWorkspace,
        A::ToggleWorkspaceFilter,
        A::PreviousWorkspace,
        A::NextWorkspace,
        A::ToggleWorkspaceCollapse,
        A::PreviousWindow,
        A::NextWindow,
    ];
    let without_default: HashSet<BuiltinAction> = BuiltinAction::all()
        .filter(|&action| default_chord(action).is_none())
        .collect();
    assert_eq!(without_default, keyless.into_iter().collect());
}

#[test]
fn a_custom_table_replaces_the_defaults_and_its_own_vacate_list() {
    let defaults = ShippedDefaults::from_table([(A::ToggleSplit, chord(CTRL | SHIFT, "e"))], vec![A::ToggleSplit]);
    assert_eq!(defaults.chord(A::ToggleSplit), Some(&chord(CTRL | SHIFT, "e")));
    assert_eq!(defaults.chord(A::NewSession), None);
    let (keymap, diagnostics) = parse_keymap("command \"Old\" ctrl+shift+e echo ok", &defaults);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        keymap.equivalent(A::ToggleSplit),
        None,
        "the command's chord vacates the new default"
    );
    assert_eq!(shortcuts(&keymap), ["ctrl+shift+e"]);
    assert_eq!(keymap.equivalent(A::NewSession), None);
}
