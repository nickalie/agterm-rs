//! The per-line pass: comment stripping, the verb dispatch and each verb's own grammar (upstream's `stripComment`,
//! `parseMapLine`, `splitMapAlternatives`, `parseCommandLine`, `parseCommandOptions`, `parseGlobalHotkeyLine`).

use super::super::BuiltinAction;
use super::super::command::CustomCommand;
use super::super::keybind::{
    Alternative, Chord, alternative_keybinds, first_word, has_key_position, has_malformed_alternative, is_arrow_key,
    is_reserved_monitor_chord, parse_keybind, trim_blank,
};
use super::{Collected, DropScope, ParsedCommandLine, ParsedMapLine};
use crate::hud::HudPosition;
use crate::model::OverlayPane;

/// The `--repeat` option of both verbs.
const REPEAT_OPTION: &str = "--repeat";
/// The failure-placement options a `command` line accepts.
const ERROR_OPTIONS: [&str; 3] = ["--error-hud", "--error-position", "--error-pane"];

impl Collected {
    /// Parses one raw line (1-based `line`), recording its result or its diagnostic.
    pub(super) fn parse_line(&mut self, line: usize, raw_line: &str) {
        let stripped = strip_comment(raw_line);
        let text = trim_blank(&stripped);
        if text.is_empty() {
            return;
        }
        let verb = first_word(text);
        let rest = trim_blank(&text[verb.len()..]);
        match verb {
            "map" => self.parse_map_line(rest, line),
            "command" => self.parse_command_line(rest, line),
            "global-hotkey" => self.parse_global_hotkey_line(rest, line),
            _ => self.diagnose(line, format!("unknown verb '{verb}'")),
        }
    }

    /// `<chord> [--repeat] <action>`. Cross-builtin collisions are left to the whole-file pass.
    fn parse_map_line(&mut self, rest: &str, line: usize) {
        let chord_text = first_word(rest);
        let mut action_name = trim_blank(&rest[chord_text.len()..]);
        let repeats = first_word(action_name) == REPEAT_OPTION;
        if repeats {
            action_name = trim_blank(&action_name[REPEAT_OPTION.len()..]);
        }
        if chord_text.is_empty() || action_name.is_empty() {
            self.diagnose(line, "map requires a chord and an action");
            return;
        }
        let Some(parsed) = alternative_keybinds(chord_text) else {
            self.diagnose(line, format!("invalid chord '{chord_text}'"));
            return;
        };
        let (chord, alternatives) = self.split_map_alternatives(parsed, line);
        // nothing survived: the per-alternative diagnostics already said why, so no action diagnostic on top.
        if chord.is_none() && alternatives.is_empty() {
            return;
        }
        let Some(action) = BuiltinAction::from_name(action_name) else {
            self.diagnose(line, format!("unknown action '{action_name}'"));
            return;
        };
        self.map_lines.push(ParsedMapLine {
            action,
            chord,
            alternatives,
            repeats,
            line,
        });
    }

    /// Sorts a `map` line's alternatives by dispatch path: the first single chord passing the menu's rules becomes
    /// the menu chord, every other one is monitor-bound and needs a modifier or function key on its first chord.
    /// A rule violation drops that alternative alone.
    fn split_map_alternatives(&mut self, parsed: Vec<Alternative>, line: usize) -> (Option<Chord>, Vec<Alternative>) {
        let scope = DropScope::new(parsed.len() > 1);
        let mut menu_chord: Option<Chord> = None;
        let mut alternatives = Vec::new();
        for alternative in parsed {
            let raw = &alternative.raw;
            if alternative.keybind.iter().any(is_reserved_monitor_chord) {
                self.diagnose(
                    line,
                    format!("chord '{raw}' is a reserved shortcut; {}", scope.map_skipped()),
                );
                continue;
            }
            if menu_chord.is_none()
                && let [chord] = alternative.keybind.as_slice()
            {
                // a bare arrow on the menu would swallow the key in the terminal, the palettes and every field.
                if is_arrow_key(&chord.key) && chord.mods.is_empty() {
                    let skipped = scope.map_skipped();
                    self.diagnose(line, format!("bare arrow chord '{raw}' needs a modifier; {skipped}"));
                } else {
                    menu_chord = Some(chord.clone());
                }
                continue;
            }
            if !alternative.keybind.first().is_some_and(Chord::can_start_shortcut) {
                let skipped = scope.map_skipped();
                self.diagnose(
                    line,
                    format!("chord '{raw}' needs a modifier on its first key; {skipped}"),
                );
                continue;
            }
            alternatives.push(alternative);
        }
        (menu_chord, alternatives)
    }

    /// `"<name>" [chord] [options] <shell...>`. A name already taken skips the line.
    fn parse_command_line(&mut self, rest: &str, line: usize) {
        let Some((name, after_name)) = rest.strip_prefix('"').and_then(|tail| tail.split_once('"')) else {
            self.diagnose(line, "command requires a quoted name");
            return;
        };
        let (kept, shell_line) = self.command_shortcut(name, trim_blank(after_name), line);
        let mut command = CustomCommand::new(name, "", "");
        match scan_command_options(shell_line, &mut command) {
            Ok(shell) => shell.clone_into(&mut command.command),
            Err(reason) => {
                self.diagnose(line, format!("command '{name}' {reason}"));
                return;
            }
        }
        // the name is the identity a run count is stored under, so a second definition cannot share it.
        if self.command_lines.iter().any(|parsed| parsed.command.name == name) {
            self.diagnose(line, format!("command '{name}' is already defined; command skipped"));
            return;
        }
        self.command_lines.push(ParsedCommandLine {
            command,
            alternatives: kept,
        });
    }

    /// The keyed alternatives of the token after a command's name, and the text left for the options and shell.
    /// The token is a binding only when something in it can start a shortcut; otherwise it stays shell text (a
    /// leading `[`, `:` or one-letter alias must not be swallowed).
    fn command_shortcut<'a>(&mut self, name: &str, after_name: &'a str, line: usize) -> (Vec<Alternative>, &'a str) {
        let first_token = first_word(after_name);
        let parsed = if first_token.is_empty() {
            None
        } else {
            alternative_keybinds(first_token)
        };
        let Some(parsed) = parsed else {
            if has_malformed_alternative(first_token) {
                let skipped = DropScope::WholeBinding.command_skipped();
                self.diagnose(
                    line,
                    format!("command '{name}' shortcut '{first_token}' has an invalid alternative; {skipped}"),
                );
            }
            return (Vec::new(), after_name);
        };
        let (kept, dropped): (Vec<Alternative>, Vec<Alternative>) = parsed
            .into_iter()
            .partition(|alternative| alternative.keybind.first().is_some_and(Chord::can_start_shortcut));
        if kept.is_empty() {
            let skipped = DropScope::WholeBinding.command_skipped();
            self.diagnose(
                line,
                format!("command '{name}' shortcut '{first_token}' must include a modifier; {skipped}"),
            );
            return (kept, after_name);
        }
        for alternative in dropped {
            let (raw, skipped) = (&alternative.raw, DropScope::Alternative.command_skipped());
            self.diagnose(
                line,
                format!("command '{name}' shortcut '{raw}' must include a modifier; {skipped}"),
            );
        }
        (kept, trim_blank(&after_name[first_token.len()..]))
    }

    /// `global-hotkey <chord>`: one chord, a modifier or function key required, its base key one a physical
    /// position produces. Repeats are last-wins.
    fn parse_global_hotkey_line(&mut self, rest: &str, line: usize) {
        match global_hotkey(rest) {
            Ok(chord) => self.global_hotkey = Some(chord),
            Err(message) => self.diagnose(line, message),
        }
    }
}

fn global_hotkey(rest: &str) -> Result<Chord, String> {
    let token = first_word(rest);
    if token.is_empty() {
        return Err("global-hotkey needs a chord".to_owned());
    }
    if !trim_blank(&rest[token.len()..]).is_empty() {
        return Err(format!("global-hotkey takes one chord, found '{rest}'"));
    }
    let chord = match parse_keybind(token) {
        Some(mut keybind) if keybind.len() == 1 => keybind.remove(0),
        _ => return Err(format!("invalid global-hotkey chord '{token}'")),
    };
    if !chord.can_start_shortcut() {
        return Err(format!("global-hotkey '{token}' must include a modifier"));
    }
    // the desktop registers a physical key position, and this line has no read-back anywhere, so a base key no
    // position produces is diagnosed here rather than silently dropped at registration.
    if !has_key_position(&chord.key) {
        return Err(format!(
            "global-hotkey '{token}' names no key position; write a shifted symbol as shift+<base>"
        ));
    }
    Ok(chord)
}

/// The options prefix of a command line, applied to `command`, and the shell text after it; `Err` carries the
/// rejection's wording after `command '<name>'`. `--` or the first ordinary word ends the options.
fn scan_command_options<'a>(body: &'a str, command: &mut CustomCommand) -> Result<&'a str, String> {
    let mut rest = body;
    let mut seen: Vec<&str> = Vec::new();
    while !rest.is_empty() {
        let option = first_word(rest);
        if option == "--" {
            rest = after_token(rest);
            break;
        }
        if option == REPEAT_OPTION {
            if command.repeats {
                return Err(format!("repeats option '{option}'"));
            }
            command.repeats = true;
            rest = after_token(rest);
            continue;
        }
        if !option.starts_with("--error-") {
            break;
        }
        rest = apply_error_option(option, after_token(rest), &mut seen, command)?;
    }
    if !command.error_hud && !seen.is_empty() {
        return Err("requires --error-hud for error placement options".to_owned());
    }
    if rest.is_empty() {
        return Err("has no shell line".to_owned());
    }
    Ok(rest)
}

/// Applies one `--error-*` option, taking its value from `rest`, and returns what follows.
fn apply_error_option<'a>(
    option: &'a str,
    rest: &'a str,
    seen: &mut Vec<&'a str>,
    command: &mut CustomCommand,
) -> Result<&'a str, String> {
    if !ERROR_OPTIONS.contains(&option) {
        return Err(format!("has unknown option '{option}'"));
    }
    if seen.contains(&option) {
        return Err(format!("repeats option '{option}'"));
    }
    seen.push(option);
    let value = first_word(rest);
    match option {
        "--error-hud" => {
            command.error_hud = true;
            Ok(rest)
        }
        "--error-position" => {
            command.error_position = HudPosition::parse(value)
                .ok_or_else(|| "requires a valid HUD position after --error-position".to_owned())?;
            Ok(after_token(rest))
        }
        _ => {
            command.error_pane =
                Some(error_pane(value).ok_or_else(|| "requires left or right after --error-pane".to_owned())?);
            Ok(after_token(rest))
        }
    }
}

/// The pane `--error-pane` names: exactly `left` or `right` (upstream's `OverlayPane(rawValue:)`, not the control
/// API's aliases).
fn error_pane(value: &str) -> Option<OverlayPane> {
    match value {
        "left" => Some(OverlayPane::Left),
        "right" => Some(OverlayPane::Right),
        _ => None,
    }
}

/// `s` past its first word and the whitespace after it.
fn after_token(s: &str) -> &str {
    s[first_word(s).len()..].trim_start_matches(char::is_whitespace)
}

/// The line up to a trailing comment: a `#` starts one only after whitespace (or at the start) and outside a
/// single- or double-quoted span, so `git commit -m 'fix #42'` keeps its `#`. The two quote states exclude each
/// other: a `"` inside `'...'` is literal, and vice versa.
pub(super) fn strip_comment(line: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    let mut previous_was_space = true;
    let mut result = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '#' if !in_single && !in_double && previous_was_space => break,
            _ => {}
        }
        result.push(c);
        previous_was_space = c.is_whitespace();
    }
    result
}
