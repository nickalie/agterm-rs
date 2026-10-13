//! The Linux build's shipped default chords. Upstream ships macOS chords built on ⌘ (Super on Linux), which desktop
//! shells claim and which collide once ⌘ becomes Ctrl+Shift, so this build ships its own table (settled with Codex,
//! recorded in the plan's Technical Details):
//!
//! - Ctrl+Shift+<letter> is the base, as Linux terminals and this build's Edit chords (`ctrl+shift+key_c/v/a`) use;
//!   the letters are those of the Linux fork this build replaces, so a chord a user already presses keeps its
//!   meaning; the chords upstream spells without ⌘ (`` ctrl+` ``, `ctrl+p`, `ctrl+shift+p`, `ctrl+shift+i`) stay;
//! - font sizes on Ctrl+=, Ctrl+-, Ctrl+0 and full screen on F11, as GTK apps do; Alt joins as a second tier where a
//!   letter is taken; the arrows avoid the Ctrl+Alt(+Shift) chords GNOME uses to switch and move workspaces;
//! - the defaults the fork did not ship (or shipped only as fixed, non-rebindable keys) give way to an explicit
//!   binding of the same chord in an existing file, as upstream does for every new default.
//!
//! The grammar is upstream's: `cmd` is the Super key and `opt` is Alt.

use super::BuiltinAction;
use super::defaults::ShippedDefaults;
use super::keybind::{Chord, Modifiers};

const CTRL_SHIFT: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);
const CTRL_ALT_SHIFT: Modifiers = CTRL_SHIFT.union(Modifiers::OPTION);
const ALT_SHIFT: Modifiers = Modifiers::OPTION.union(Modifiers::SHIFT);

/// The Linux table: every action upstream ships with a key has one here.
#[must_use]
pub fn linux_defaults() -> ShippedDefaults {
    ShippedDefaults::from_table(linux_table(), linux_vacated().to_vec())
}

fn linux_table() -> Vec<(BuiltinAction, Chord)> {
    use BuiltinAction as A;
    let chord = |mods: Modifiers, key: &str| Chord::new(mods, key);
    vec![
        (A::NewWindow, chord(CTRL_SHIFT, "n")),
        (A::NewWorkspace, chord(CTRL_SHIFT, "w")),
        (A::NewSession, chord(CTRL_SHIFT, "t")),
        (A::OpenDirectory, chord(CTRL_SHIFT, "o")),
        (A::CloseSession, chord(CTRL_SHIFT, "q")),
        (A::ReopenRecent, chord(CTRL_ALT_SHIFT, "t")),
        (A::UndoClose, chord(CTRL_SHIFT, "z")),
        (A::IncreaseFontSize, chord(Modifiers::CONTROL, "=")),
        (A::DecreaseFontSize, chord(Modifiers::CONTROL, "-")),
        (A::ResetFontSize, chord(Modifiers::CONTROL, "0")),
        (A::ToggleSplit, chord(CTRL_SHIFT, "d")),
        (A::ToggleHorizontalSplit, chord(CTRL_SHIFT, "h")),
        (A::ToggleScratch, chord(CTRL_SHIFT, "j")),
        (A::ToggleTerminalZoom, chord(CTRL_SHIFT, "return")),
        (A::ToggleSearch, chord(CTRL_SHIFT, "f")),
        (A::ToggleSidebar, chord(CTRL_SHIFT, "s")),
        (A::ToggleFullscreen, chord(Modifiers::empty(), "f11")),
        (A::ToggleFlag, chord(CTRL_SHIFT, "g")),
        (A::QuickTerminal, chord(Modifiers::CONTROL, "`")),
        (A::SessionPalette, chord(Modifiers::CONTROL, "p")),
        (A::CommandPalette, chord(CTRL_SHIFT, "p")),
        (A::CustomCommandPalette, chord(CTRL_ALT_SHIFT, "o")),
        (A::ShowAttention, chord(CTRL_SHIFT, "i")),
        (A::Dashboard, chord(CTRL_SHIFT, "m")),
        (A::AttachRemote, chord(CTRL_SHIFT, "r")),
        (A::FocusLeftPane, chord(CTRL_SHIFT, "left")),
        (A::FocusRightPane, chord(CTRL_SHIFT, "right")),
        (A::PreviousSession, chord(CTRL_SHIFT, "up")),
        (A::NextSession, chord(CTRL_SHIFT, "down")),
        (A::PreviousAttentionSession, chord(ALT_SHIFT, "up")),
        (A::NextAttentionSession, chord(ALT_SHIFT, "down")),
    ]
}

/// The defaults new relative to the Linux fork (or fixed there), plus the fork's own `attach_remote` exception.
fn linux_vacated() -> [BuiltinAction; 15] {
    use BuiltinAction as A;
    [
        A::ReopenRecent,
        A::UndoClose,
        A::IncreaseFontSize,
        A::DecreaseFontSize,
        A::ResetFontSize,
        A::ToggleTerminalZoom,
        A::ToggleFullscreen,
        A::CustomCommandPalette,
        A::FocusLeftPane,
        A::FocusRightPane,
        A::PreviousSession,
        A::NextSession,
        A::PreviousAttentionSession,
        A::NextAttentionSession,
        A::AttachRemote,
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::keymap::{is_reserved_monitor_chord, parse_keybind, parse_keymap};

    #[test]
    fn every_action_upstream_keys_has_a_distinct_linux_default() {
        let upstream = ShippedDefaults::upstream();
        let linux = linux_defaults();
        let mut seen = HashSet::new();
        for action in BuiltinAction::all() {
            assert_eq!(
                upstream.chord(action).is_some(),
                linux.chord(action).is_some(),
                "{}",
                action.name()
            );
            if let Some(chord) = linux.chord(action) {
                assert!(
                    seen.insert(chord.clone()),
                    "{} shares {}",
                    action.name(),
                    chord.display_string()
                );
                assert!(!is_reserved_monitor_chord(chord), "{}", action.name());
                assert!(
                    !chord.mods.contains(Modifiers::COMMAND),
                    "{} needs Super",
                    action.name()
                );
                // each default round-trips through the grammar, so the starter file can spell it
                assert_eq!(
                    parse_keybind(&chord.display_string()),
                    Some(vec![chord.clone()]),
                    "{}",
                    action.name()
                );
            }
        }
        assert_eq!(seen.len(), 31);
    }

    #[test]
    fn an_empty_file_resolves_every_default() {
        let (keymap, diagnostics) = parse_keymap("", &linux_defaults());
        assert!(diagnostics.is_empty());
        assert_eq!(
            keymap
                .equivalent(BuiltinAction::CloseSession)
                .map(Chord::display_string)
                .as_deref(),
            Some("ctrl+shift+q")
        );
        assert_eq!(
            keymap
                .equivalent(BuiltinAction::PreviousAttentionSession)
                .map(Chord::display_string)
                .as_deref(),
            Some("opt+shift+up")
        );
    }

    #[test]
    fn a_file_using_a_new_default_keeps_its_binding() {
        // the fork shipped no undo_close default, so a custom command there keeps the chord
        let (keymap, diagnostics) = parse_keymap("command \"mark\" ctrl+shift+z true\n", &linux_defaults());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(keymap.equivalent(BuiltinAction::UndoClose), None);
        assert_eq!(keymap.commands()[0].shortcut, "ctrl+shift+z");
        // a default the fork already shipped still wins over a custom command
        let (keymap, diagnostics) = parse_keymap("command \"mark\" ctrl+shift+t true\n", &linux_defaults());
        assert_eq!(diagnostics.len(), 1);
        assert!(keymap.commands()[0].shortcut.is_empty());
        assert!(keymap.equivalent(BuiltinAction::NewSession).is_some());
    }

    #[test]
    fn the_users_fork_keymap_parses_clean() {
        let text = "command \"Open in Zed\" ctrl+shift+b /usr/bin/zeditor \"$AGT_SESSION_PWD\"\n\
                    command \"Open in Thunar\" ctrl+shift+e /usr/bin/thunar \"$AGT_SESSION_PWD\"\n";
        let (keymap, diagnostics) = parse_keymap(text, &linux_defaults());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(keymap.commands().iter().all(|command| !command.shortcut.is_empty()));
    }
}
