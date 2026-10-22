//! The shipped default chord table the parser resolves against (upstream's `BuiltinAction.defaultChord`), held as
//! data so a build can ship its own table, plus the actions whose default is vacated when an existing file
//! already uses that chord (upstream's compatibility shims in `parseKeymap`).

use std::collections::BTreeMap;

use super::BuiltinAction;
use super::keybind::{Chord, Modifiers};

/// The default chord of each keyed built-in, and the actions that give theirs up for compatibility.
///
/// The table's chords must be distinct: the built-in collision fixpoint relies on every collision involving a user
/// override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippedDefaults {
    chords: BTreeMap<BuiltinAction, Chord>,
    vacate_when_used: Vec<BuiltinAction>,
}

impl ShippedDefaults {
    /// Upstream agterm's macOS table verbatim, vacating `toggle_horizontal_split`, `dashboard` and
    /// `attach_remote` (each a default added after files could already bind its chord).
    #[must_use]
    pub fn upstream() -> Self {
        Self::from_table(
            upstream_table(),
            vec![
                BuiltinAction::ToggleHorizontalSplit,
                BuiltinAction::Dashboard,
                BuiltinAction::AttachRemote,
            ],
        )
    }

    /// A table of `(action, default chord)` pairs (a later pair for the same action wins) and the actions to vacate:
    /// one of those whose default chord the file uses elsewhere, while it does not map the action itself, goes
    /// unbound rather than invalidating the file.
    #[must_use]
    pub fn from_table(
        chords: impl IntoIterator<Item = (BuiltinAction, Chord)>,
        vacate_when_used: Vec<BuiltinAction>,
    ) -> Self {
        Self {
            chords: chords.into_iter().collect(),
            vacate_when_used,
        }
    }

    /// The action's default chord, `None` for a keyless action.
    #[must_use]
    pub fn chord(&self, action: BuiltinAction) -> Option<&Chord> {
        self.chords.get(&action)
    }

    /// The actions whose default goes unbound when a file explicitly uses it elsewhere, in table order.
    #[must_use]
    pub fn vacate_when_used(&self) -> &[BuiltinAction] {
        &self.vacate_when_used
    }
}

fn upstream_table() -> Vec<(BuiltinAction, Chord)> {
    use BuiltinAction as A;
    let cmd = Modifiers::COMMAND;
    let ctrl = Modifiers::CONTROL;
    let opt = Modifiers::OPTION;
    let shift = Modifiers::SHIFT;
    [
        (A::NewWindow, cmd | opt, "n"),
        (A::NewWorkspace, cmd | shift, "n"),
        (A::NewSession, cmd, "n"),
        (A::OpenDirectory, cmd, "o"),
        (A::CloseSession, cmd, "w"),
        (A::ReopenRecent, cmd | shift, "t"),
        (A::UndoClose, cmd, "z"),
        (A::IncreaseFontSize, cmd, "+"),
        (A::DecreaseFontSize, cmd, "-"),
        (A::ResetFontSize, cmd, "0"),
        (A::ToggleSplit, cmd, "d"),
        (A::ToggleHorizontalSplit, cmd | shift, "d"),
        (A::ToggleScratch, cmd, "j"),
        (A::ToggleTerminalZoom, cmd | shift, "return"),
        (A::ToggleSearch, cmd, "f"),
        (A::ToggleSidebar, cmd | ctrl, "s"),
        (A::ToggleFullscreen, cmd | ctrl, "f"),
        (A::ToggleFlag, cmd | shift, "f"),
        (A::QuickTerminal, ctrl, "`"),
        (A::SessionPalette, ctrl, "p"),
        (A::CommandPalette, ctrl | shift, "p"),
        (A::CustomCommandPalette, ctrl | shift, "o"),
        (A::ShowAttention, ctrl | shift, "i"),
        (A::Dashboard, cmd | shift, "g"),
        (A::AttachRemote, cmd | shift, "r"),
        (A::FocusLeftPane, cmd | opt, "left"),
        (A::FocusRightPane, cmd | opt, "right"),
        (A::PreviousSession, cmd | opt, "up"),
        (A::NextSession, cmd | opt, "down"),
        (A::PreviousAttentionSession, ctrl | opt, "up"),
        (A::NextAttentionSession, ctrl | opt, "down"),
    ]
    .into_iter()
    .map(|(action, mods, key)| (action, Chord::new(mods, key)))
    .collect()
}
