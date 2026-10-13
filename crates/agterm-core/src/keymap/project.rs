//! The `keymap.list` read-back (upstream's `ControlKeymap.project`): the parsed keymap and its diagnostics as the
//! wire payload, the live menu supplied separately by the frontend.

use agterm_proto::keymap::{
    ControlKeymap, ControlKeymapAction, ControlKeymapCommand, ControlKeymapDiagnostic, ControlKeymapMenuItem,
};

use super::command::CustomCommand;
use super::keybind::keybind_display_string;
use super::parsed::Keymap;
use super::{BuiltinAction, KeymapDiagnostic};

/// Projects a parsed keymap into the `keymap.list` payload. Actions come out in [`BuiltinAction`] declaration order,
/// grouped as the starter file lists them; `overridden` compares the resolved menu chord with the shipped default,
/// so an identity `map` line is not an override.
#[must_use]
pub fn project(
    keymap: &Keymap,
    diagnostics: &[KeymapDiagnostic],
    path: &str,
    menu: Option<Vec<ControlKeymapMenuItem>>,
) -> ControlKeymap {
    ControlKeymap {
        path: path.to_owned(),
        actions: BuiltinAction::all()
            .map(|action| project_action(keymap, action))
            .collect(),
        commands: keymap.commands().iter().map(project_command).collect(),
        diagnostics: diagnostics
            .iter()
            .map(|diagnostic| ControlKeymapDiagnostic {
                line: i64::try_from(diagnostic.line).unwrap_or(i64::MAX),
                message: diagnostic.message.clone(),
            })
            .collect(),
        menu,
    }
}

fn project_action(keymap: &Keymap, action: BuiltinAction) -> ControlKeymapAction {
    let resolved = keymap.equivalent(action);
    let alternates: Vec<String> = keymap
        .sequences(action)
        .iter()
        .map(|keybind| keybind_display_string(keybind))
        .collect();
    ControlKeymapAction {
        action: action.name().to_owned(),
        chord: resolved.map(super::keybind::Chord::display_string),
        alternates: (!alternates.is_empty()).then_some(alternates),
        overridden: (resolved != keymap.defaults().chord(action)).then_some(true),
        repeats: keymap.builtin_repeating().contains(&action).then_some(true),
    }
}

fn project_command(command: &CustomCommand) -> ControlKeymapCommand {
    ControlKeymapCommand {
        name: command.name.clone(),
        shortcut: (!command.shortcut.is_empty()).then(|| command.shortcut.clone()),
        error_hud: command.error_hud,
        error_position: command.error_position.wire_name().to_owned(),
        error_pane: command.error_pane.map(|pane| pane.wire_name().to_owned()),
        repeats: command.repeats,
    }
}
