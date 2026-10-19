//! The host-free transforms of the agent-status hooks install (upstream's `AgentHooksInstall.swift`): idempotent
//! string, JSON and TOML rewrites returning the new contents and whether anything changed. The installer
//! (`hooks.rs`) owns the reads, writes and backups.

use serde_json::{Map, Value};

/// The wrapper script the hooks invoke, installed into the script directory.
pub const WRAPPER_NAME: &str = "agterm-agent-status.sh";

/// The Codex lifecycle adapter installed beside the generic wrapper.
pub const CODEX_WRAPPER_NAME: &str = "agterm-codex-status.sh";

/// The Claude adapter the Claude hooks invoke instead of the generic wrapper: it answers from process topology whether
/// the firing agent owns the pane, then delegates to the generic wrapper.
pub const CLAUDE_WRAPPER_NAME: &str = "agterm-claude-status.sh";

/// The bundled Pi extension's path inside the package, its destination name and its ownership sentinel.
pub const PI_EXTENSION_RELATIVE_PATH: &str = "pi/agterm-status.ts";
pub const PI_EXTENSION_NAME: &str = "agterm-status.ts";
pub const PI_EXTENSION_MARKER: &str = "// agterm-pi-status-extension";

/// The shell integration scripts sourced from the rc files, relative to the script directory.
pub const INTEGRATION_RELATIVE_PATH: &str = "shell/integration.sh";
pub const FISH_INTEGRATION_RELATIVE_PATH: &str = "shell/integration.fish";

/// The sentinel opening the installer-baked `AGTERMCTL` block; a re-bake replaces it instead of stacking another.
pub const AGTERMCTL_MARKER: &str = "# >>> agterm ngtermctl path (installer-baked) >>>";

/// The markers bracketing the agterm-managed block in a shell rc file and in Codex's `config.toml`; the opening one is
/// also the idempotency probe.
pub const RC_MARKER_BEGIN: &str = "# >>> agterm agent-status >>>";
pub const RC_MARKER_END: &str = "# <<< agterm agent-status <<<";

/// The Claude Code hook events the merge installs, each with its matcher and the state (plus flags) it reports.
/// `PostToolUse` re-asserts `active` after every tool, so a status returns to active when work resumes after a
/// permission prompt: Claude Code has no "permission answered" event, and the gated tool's `PreToolUse` fired before
/// `blocked` was set, so its `PostToolUse` is the first hook afterwards. `PermissionRequest` sets `blocked` as the
/// prompt opens; `Notification[permission_prompt]` fires only once the prompt has waited about six seconds with no
/// keystroke, and stays for the sandboxed network prompt, which `PermissionRequest` does not cover. `SessionEnd` is
/// this build's (upstream installs the other five): the adapter turns a `Stop` with background work in flight into
/// `active`, which only a later `Stop` replaces, so an agent quit before that work finished would leave the row
/// pulsing; it ends as a plain `Stop` would. `Notification` alone carries a matcher, and only `Stop` and `SessionEnd`
/// pass `--auto-reset` (it clears on visit); the rest stay keep-state.
const CLAUDE_HOOKS: [(&str, Option<&str>, &str); 6] = [
    ("UserPromptSubmit", None, "active --blink"),
    ("PostToolUse", None, "active --blink"),
    ("Stop", None, "completed --auto-reset"),
    ("PermissionRequest", None, "blocked"),
    ("Notification", Some("permission_prompt"), "blocked"),
    ("SessionEnd", None, "completed --auto-reset"),
];

/// Whether an earlier installer (this build's before `SessionEnd`, the fork's, upstream's) wrote `event`: only those
/// entries are its own to migrate, so a `SessionEnd` entry naming a wrapper is always the user's.
fn written_by_earlier_installs(event: &str) -> bool {
    event != "SessionEnd"
}

/// The Codex lifecycle events and the action the installed Codex adapter takes for each.
const CODEX_HOOKS: [(&str, &str); 6] = [
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "user-prompt-submit"),
    ("PreToolUse", "pre-tool-use"),
    ("PostToolUse", "post-tool-use"),
    ("PermissionRequest", "permission-request"),
    ("Stop", "stop"),
];

/// The existing `settings.json` is non-empty but not a JSON object, or a `hooks` value the merge writes has the wrong
/// shape: the installer refuses to overwrite what it could not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedSettings;

/// The Pi extension's destination under `home`.
#[must_use]
pub fn pi_extension_path(home: &str) -> String {
    format!("{home}/.pi/agent/extensions/{PI_EXTENSION_NAME}")
}

/// Whether the Pi extension destination may be replaced: absent is safe, an existing file must carry the ownership
/// marker, an unreadable one counts as the user's.
#[must_use]
pub fn may_overwrite_pi_extension(file_exists: bool, existing: Option<&str>) -> bool {
    !file_exists || existing.is_some_and(|text| text.contains(PI_EXTENSION_MARKER))
}

/// Merges the agent-status hooks into a Claude Code `settings.json` (upstream's `mergeClaudeSettings`). `existing`
/// `None` or blank starts a fresh object. Idempotent: hooks already present (either wrapper named) leave the input
/// unchanged. Entries an earlier install pointed straight at the generic wrapper move onto the Claude adapter first.
///
/// `legacy_dirs` are script directories an earlier install used (the fork's, and this build's before it took a
/// directory of its own, `hooks::legacy_script_dir`): an entry calling one of our wrappers there is repointed at
/// `script_dir`, or dropped when it is exactly what that install generated and its event already calls `script_dir`, so
/// a reinstall moves the hooks instead of doubling them (Linux glue: upstream has one directory).
///
/// # Errors
///
/// [`MalformedSettings`] when `existing` is not a JSON object, or its `hooks` or an event the merge writes is
/// wrong-shaped.
pub fn merge_claude_settings(
    existing: Option<&str>,
    script_dir: &str,
    legacy_dirs: &[&str],
) -> Result<(String, bool), MalformedSettings> {
    let mut root = parsed_object(existing)?;
    let mut hooks = claude_hooks_object(&root)?;
    let mut changed = migrate_claude_entries_from_legacy_dirs(&mut hooks, script_dir, legacy_dirs);
    changed |= migrate_claude_entries_to_adapter(&mut hooks, script_dir);
    let command = wrapper_command(script_dir);
    for (event, matcher, state) in CLAUDE_HOOKS {
        let entries = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(entries) = entries else {
            continue; // the shape was checked above
        };
        if entries.iter().any(|entry| entry_uses_wrapper(entry, script_dir)) {
            continue;
        }
        entries.push(hook_entry(&command, state, matcher));
        changed = true;
    }
    if !changed {
        return Ok((existing.unwrap_or_default().to_owned(), false));
    }
    root.insert("hooks".to_owned(), Value::Object(hooks));
    Ok((serialize(&Value::Object(root)), true))
}

/// Appends the marker-guarded `source` line for the shell integration to a shell rc file (upstream's
/// `appendShellRC`). Idempotent: a begin marker already present keeps `existing`, except that a managed block sourcing
/// `script_name` from another directory (an earlier install's, the fork's) is rewritten in place to source it from
/// `script_dir` (Linux glue: upstream has one directory, so it never rewrites).
#[must_use]
pub fn append_shell_rc(existing: &str, script_dir: &str, script_name: &str) -> (String, bool) {
    let source = format!("source {}", shell_quote(&format!("{script_dir}/{script_name}")));
    if existing.contains(RC_MARKER_BEGIN) {
        let refreshed = refresh_rc_block(existing, &source, script_name);
        let changed = refreshed != existing;
        return (refreshed, changed);
    }
    let block = format!("{RC_MARKER_BEGIN}\n{source}\n{RC_MARKER_END}\n");
    if existing.is_empty() {
        return (block, true);
    }
    let mut contents = existing.to_owned();
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push('\n');
    contents.push_str(&block);
    (contents, true)
}

/// What merging the Codex hooks into `~/.codex/config.toml` decided, from parsing the file first (upstream's
/// `CodexMergeOutcome`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexMerge {
    /// The hooks block was added or refreshed (and a retired `codex-notify.sh` notify line removed): write it.
    Merged(String),
    /// The file already carries the current block.
    Unchanged,
    /// The file defines its own `hooks`: appending would duplicate or break them.
    HooksExist,
    /// The file is not valid TOML.
    Unparseable,
}

/// Merges the Codex lifecycle hooks into an existing `config.toml` (`existing` empty: no file yet), as upstream's
/// `mergeCodexConfig`: a managed block is refreshed in place keeping Codex's trust-state tables, a file that does
/// not parse or defines its own hooks is left alone, otherwise the block is appended surgically.
#[must_use]
pub fn merge_codex_config(existing: &str, script_dir: &str) -> CodexMerge {
    if existing.contains(RC_MARKER_BEGIN) {
        let refreshed = refresh_managed_codex_block(existing, script_dir);
        return if refreshed == existing {
            CodexMerge::Unchanged
        } else {
            CodexMerge::Merged(refreshed)
        };
    }
    if existing.trim().is_empty() {
        return CodexMerge::Merged(append_codex_block(existing, script_dir));
    }
    let Ok(table) = existing.parse::<toml::Table>() else {
        return CodexMerge::Unparseable;
    };
    if table.contains_key("hooks") {
        return CodexMerge::HooksExist;
    }
    let notify: Vec<&str> = match table.get("notify") {
        Some(toml::Value::String(single)) => vec![single.as_str()],
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(toml::Value::as_str)
            .collect::<Option<_>>()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    // the removal is a one-line cut: a notify array a user spread over several lines would leave its tail behind as
    // invalid TOML, so such a file keeps its notify line and only gains the block
    let text = notify
        .iter()
        .any(|program| program.contains("codex-notify.sh"))
        .then(|| remove_legacy_codex_notify(existing))
        .filter(|removed| removed.parse::<toml::Table>().is_ok())
        .unwrap_or_else(|| existing.to_owned());
    CodexMerge::Merged(append_codex_block(&text, script_dir))
}

/// Bakes `tool_path`, the bundled `ngtermctl`, into an installed wrapper, replacing the block an earlier install left
/// (upstream's `bakeAgtermctlPath`). An explicit `AGTERMCTL` is never second-guessed; a baked path that is gone falls
/// back to `ngtermctl` on `PATH` (`cli::TOOL_NAME`).
#[must_use]
pub fn bake_ngtermctl_path(text: &str, tool_path: &str) -> String {
    let block = [
        AGTERMCTL_MARKER.to_owned(),
        "if [ -z \"${AGTERMCTL:-}\" ]; then".to_owned(),
        format!("  AGTERMCTL={}", shell_quote(tool_path)),
        "  [ -x \"$AGTERMCTL\" ] || AGTERMCTL=\"$(command -v ngtermctl || true)\"".to_owned(),
        "fi".to_owned(),
    ];
    let stripped = strip_baked_block(text, block.len() - 1);
    let mut lines: Vec<String> = stripped.split('\n').map(str::to_owned).collect();
    let at = usize::from(lines.first().is_some_and(|line| line.starts_with("#!")));
    lines.splice(at..at, block);
    lines.join("\n")
}

/// The backup path: `.bak` appended to the whole name (`settings.json.bak`).
#[must_use]
pub fn backup_path(path: &str) -> String {
    format!("{path}.bak")
}

/// The `[[hooks.*]]` block the Codex merge writes, one table pair per lifecycle event, the adapter's path
/// shell-quoted inside a TOML basic string (upstream's `codexHooksBlock`).
#[must_use]
pub fn codex_hooks_block(script_dir: &str) -> String {
    let wrapper = shell_quote(&format!("{script_dir}/{CODEX_WRAPPER_NAME}"));
    CODEX_HOOKS
        .iter()
        .map(|(event, action)| {
            format!(
                "[[hooks.{event}]]\n[[hooks.{event}.hooks]]\ntype = \"command\"\ncommand = {}",
                toml_basic_string(&format!("{wrapper} {action}"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Single-quotes `value` for `/bin/sh`.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn toml_basic_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn wrapper_path(script_dir: &str) -> String {
    format!("{script_dir}/{WRAPPER_NAME}")
}

fn claude_wrapper_path(script_dir: &str) -> String {
    format!("{script_dir}/{CLAUDE_WRAPPER_NAME}")
}

/// The start of a Claude hook's command: the quoted Claude adapter, then the state.
fn wrapper_command(script_dir: &str) -> String {
    format!("{} ", shell_quote(&claude_wrapper_path(script_dir)))
}

/// A blank or absent text is a fresh object; anything else must parse as a JSON object.
fn parsed_object(text: Option<&str>) -> Result<Map<String, Value>, MalformedSettings> {
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return Ok(Map::new());
    };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(MalformedSettings),
    }
}

/// The `hooks` object, refusing a wrong-shaped one or a wrong-shaped value under an event the merge writes: reading
/// those as absent would write over them. An unrelated event of any shape round-trips.
fn claude_hooks_object(root: &Map<String, Value>) -> Result<Map<String, Value>, MalformedSettings> {
    let Some(value) = root.get("hooks") else {
        return Ok(Map::new());
    };
    let Value::Object(hooks) = value else {
        return Err(MalformedSettings);
    };
    for (event, _, _) in CLAUDE_HOOKS {
        if let Some(entries) = hooks.get(event)
            && !is_array_of_objects(entries)
        {
            return Err(MalformedSettings);
        }
    }
    Ok(hooks.clone())
}

fn is_array_of_objects(value: &Value) -> bool {
    value.as_array().is_some_and(|items| items.iter().all(Value::is_object))
}

/// Repoints an earlier install's entries from the generic wrapper at the Claude adapter, returning whether anything
/// moved. Byte-exact against the command this installer generated for that event (the pre-`--blink` prompt form
/// included), so a customized entry or a user's own hook naming the wrapper is left alone; only the command string
/// changes, a matcher and sibling keys survive.
fn migrate_claude_entries_to_adapter(hooks: &mut Map<String, Value>, script_dir: &str) -> bool {
    let generated = format!("{} ", shell_quote(&wrapper_path(script_dir)));
    let replacement = wrapper_command(script_dir);
    let mut changed = false;
    for (event, _, state) in CLAUDE_HOOKS {
        if !written_by_earlier_installs(event) {
            continue;
        }
        let Some(Value::Array(entries)) = hooks.get_mut(event) else {
            continue;
        };
        let states: &[&str] = if event == "UserPromptSubmit" {
            &[state, "active"]
        } else {
            &[state]
        };
        for entry in entries.iter_mut() {
            changed |= migrate_entry(entry, &generated, states, &format!("{replacement}{state}"));
        }
    }
    changed
}

fn migrate_entry(entry: &mut Value, generated: &str, states: &[&str], replacement: &str) -> bool {
    let Some(Value::Array(commands)) = entry.get_mut("hooks") else {
        return false;
    };
    if !commands.iter().all(Value::is_object) {
        return false;
    }
    let mut changed = false;
    for command in commands.iter_mut() {
        let Some(slot) = command.get_mut("command") else {
            continue;
        };
        if states
            .iter()
            .any(|state| slot.as_str() == Some(format!("{generated}{state}").as_str()))
        {
            *slot = Value::String(replacement.to_owned());
            changed = true;
        }
    }
    changed
}

/// Moves the entries an earlier install wrote under one of `legacy_dirs` onto `script_dir`, returning whether anything
/// changed. In every event (an older install may have wired one this merge no longer writes): an entry that is exactly
/// the stock entry an earlier install generated for that event in a legacy directory is dropped when the event already
/// calls `script_dir` under the same matcher; any other entry calling a legacy wrapper (a user's own command, matcher, event or keys) has its
/// commands repointed in place, keeping a matcher, sibling keys and the user's own commands beside them.
fn migrate_claude_entries_from_legacy_dirs(
    hooks: &mut Map<String, Value>,
    script_dir: &str,
    legacy_dirs: &[&str],
) -> bool {
    let moves = legacy_moves(script_dir, legacy_dirs);
    if moves.is_empty() {
        return false;
    }
    let mut changed = false;
    for (event, entries) in hooks.iter_mut() {
        let Value::Array(entries) = entries else {
            continue;
        };
        // a stock legacy entry is replaced only by a current one under the same matcher: another matcher's entry
        // covers other notifications, and the merge below would not add the stock one back beside it
        let current_matchers: Vec<Option<Value>> = entries
            .iter()
            .filter(|entry| entry_uses_wrapper(entry, script_dir))
            .map(|entry| entry.get("matcher").cloned())
            .collect();
        let before = entries.len();
        entries.retain(|entry| {
            !(current_matchers.contains(&entry.get("matcher").cloned())
                && is_generated_legacy_entry(event, entry, &moves))
        });
        changed |= entries.len() != before;
        for entry in entries.iter_mut() {
            changed |= repoint_entry(entry, &moves);
        }
    }
    changed
}

/// Each wrapper's path in a legacy directory and in `script_dir`, both as they read inside a shell-quoted command.
fn legacy_moves(script_dir: &str, legacy_dirs: &[&str]) -> Vec<(String, String)> {
    let quoted_inner = |path: String| path.replace('\'', "'\\''");
    legacy_dirs
        .iter()
        .filter(|legacy| **legacy != script_dir)
        .flat_map(|legacy| {
            [CLAUDE_WRAPPER_NAME, WRAPPER_NAME].map(|name| {
                (
                    quoted_inner(format!("{legacy}/{name}")),
                    quoted_inner(format!("{script_dir}/{name}")),
                )
            })
        })
        .collect()
}

/// Whether `entry` under `event` is exactly the stock entry an earlier install generated in a legacy directory: the
/// event's own matcher (or none) and no other keys, and every hook (one at least) a bare `{type: command, command}`
/// running `'<legacy wrapper>' <state>` with that event's state (`UserPromptSubmit` also its pre-`--blink` form), as
/// [`migrate_claude_entries_to_adapter`] scopes it. Anything a user changed (a chained command, another matcher or
/// event, an added key) is repointed and kept rather than dropped.
fn is_generated_legacy_entry(event: &str, entry: &Value, moves: &[(String, String)]) -> bool {
    if !written_by_earlier_installs(event) {
        return false;
    }
    let Some((_, matcher, state)) = CLAUDE_HOOKS.iter().find(|(name, _, _)| *name == event) else {
        return false;
    };
    let Some(object) = entry.as_object() else {
        return false;
    };
    let keys_are_stock = object.keys().all(|key| key == "hooks" || key == "matcher");
    let matcher_is_stock = match (object.get("matcher"), matcher) {
        (None, None) => true,
        (Some(found), Some(stock)) => found.as_str() == Some(*stock),
        _ => false,
    };
    let states: &[&str] = if event == "UserPromptSubmit" {
        &[state, "active"]
    } else {
        &[state]
    };
    let Some(hooks) = object.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    keys_are_stock
        && matcher_is_stock
        && !hooks.is_empty()
        && hooks.iter().all(|hook| is_generated_legacy_hook(hook, states, moves))
}

/// Whether `hook` is exactly `{type: command, command: '<legacy wrapper>' <state>}` for one of `states`.
fn is_generated_legacy_hook(hook: &Value, states: &[&str], moves: &[(String, String)]) -> bool {
    let Some(object) = hook.as_object() else {
        return false;
    };
    let Some(command) = object.get("command").and_then(Value::as_str) else {
        return false;
    };
    object.len() == 2
        && object.get("type").and_then(Value::as_str) == Some("command")
        && moves.iter().any(|(legacy, _)| {
            command
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_prefix(legacy.as_str()))
                .and_then(|rest| rest.strip_prefix("' "))
                .is_some_and(|found| states.contains(&found))
        })
}

/// Repoints the commands of `entry` that call a wrapper in a legacy directory, returning whether any did.
fn repoint_entry(entry: &mut Value, moves: &[(String, String)]) -> bool {
    let Some(Value::Array(commands)) = entry.get_mut("hooks") else {
        return false;
    };
    if !commands.iter().all(Value::is_object) {
        return false;
    }
    let mut changed = false;
    for command in commands.iter_mut() {
        let Some(Value::String(text)) = command.get_mut("command") else {
            continue;
        };
        for (legacy, current) in moves {
            if text.contains(legacy.as_str()) {
                *text = text.replace(legacy.as_str(), current);
                changed = true;
            }
        }
    }
    changed
}

/// `{ (matcher?), hooks: [{ type: command, command }] }`.
fn hook_entry(command: &str, state: &str, matcher: Option<&str>) -> Value {
    let mut hook = Map::new();
    hook.insert("type".to_owned(), Value::String("command".to_owned()));
    hook.insert("command".to_owned(), Value::String(format!("{command}{state}")));
    let mut entry = Map::new();
    entry.insert("hooks".to_owned(), Value::Array(vec![Value::Object(hook)]));
    if let Some(matcher) = matcher {
        entry.insert("matcher".to_owned(), Value::String(matcher.to_owned()));
    }
    Value::Object(entry)
}

/// Whether an entry already invokes us: either the adapter or the generic wrapper counts, so a customized wrapper
/// entry the migration declined is not doubled by a stock one.
fn entry_uses_wrapper(entry: &Value, script_dir: &str) -> bool {
    let probes = [claude_wrapper_path(script_dir), wrapper_path(script_dir)];
    let Some(commands) = entry.get("hooks").filter(|hooks| is_array_of_objects(hooks)) else {
        return false;
    };
    commands.as_array().into_iter().flatten().any(|command| {
        command
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| probes.iter().any(|probe| command.contains(probe.as_str())))
    })
}

/// Pretty-printed JSON with every object's keys sorted (deterministic for diffs), newline-terminated.
fn serialize(value: &Value) -> String {
    serde_json::to_string_pretty(&sorted(value)).map_or_else(|_| "{}".to_owned(), |text| text + "\n")
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|key| (key.clone(), sorted(&map[key]))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Swift's `.whitespaces`: horizontal whitespace, not line breaks.
fn trim_horizontal(line: &str) -> &str {
    line.trim_matches(|ch: char| ch.is_whitespace() && !matches!(ch, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}'))
}

/// Replaces the body of the managed rc block with `source` when it sources `script_name` (from any directory); a block
/// without its end marker, or naming no integration script of ours, is left as it is.
fn refresh_rc_block(text: &str, source: &str, script_name: &str) -> String {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let Some(begin) = lines.iter().position(|line| trim_horizontal(line) == RC_MARKER_BEGIN) else {
        return text.to_owned();
    };
    let Some(end) = (begin + 1..lines.len()).find(|&index| trim_horizontal(lines[index]) == RC_MARKER_END) else {
        return text.to_owned();
    };
    let body = &lines[begin + 1..end];
    if body == [source] || !body.iter().any(|line| line.contains(script_name)) {
        return text.to_owned();
    }
    lines.splice(begin + 1..end, [source]);
    lines.join("\n")
}

fn append_codex_block(text: &str, script_dir: &str) -> String {
    let block = format!(
        "{RC_MARKER_BEGIN}\n{}\n{RC_MARKER_END}\n",
        codex_hooks_block(script_dir)
    );
    if text.is_empty() {
        return block;
    }
    let mut contents = text.to_owned();
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push('\n');
    contents + &block
}

/// Replaces only the generated definitions inside an existing managed block. Codex writes its `[hooks.state…]` trust
/// records at the end of the file, inside our end marker, so that suffix is kept byte for byte. A marker block naming
/// none of our scripts is foreign and left untouched.
fn refresh_managed_codex_block(text: &str, script_dir: &str) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let Some(begin) = lines.iter().position(|line| trim_horizontal(line) == RC_MARKER_BEGIN) else {
        return text.to_owned();
    };
    let Some(end) = (begin + 1..lines.len()).find(|&index| trim_horizontal(&lines[index]) == RC_MARKER_END) else {
        return text.to_owned();
    };
    let body = &lines[begin + 1..end];
    if !body
        .iter()
        .any(|line| line.contains(WRAPPER_NAME) || line.contains(CODEX_WRAPPER_NAME))
    {
        return text.to_owned();
    }
    let mut suffix: Vec<String> = Vec::new();
    if let Some(offset) = body
        .iter()
        .position(|line| trim_horizontal(line).starts_with("[hooks.state"))
    {
        let mut start = begin + 1 + offset;
        if start > begin + 1 && trim_horizontal(&lines[start - 1]).is_empty() {
            start -= 1;
        }
        suffix = lines[start..end].to_vec();
    }
    let mut replacement: Vec<String> = codex_hooks_block(script_dir).split('\n').map(str::to_owned).collect();
    if let Some(first) = suffix.first() {
        if !trim_horizontal(first).is_empty() {
            replacement.push(String::new());
        }
        replacement.extend(suffix);
    }
    lines.splice(begin + 1..end, replacement);
    lines.join("\n")
}

/// Removes the retired single-line `notify = [...codex-notify.sh...]`, only above the first table header and only
/// where the key is `notify` itself.
fn remove_legacy_codex_notify(text: &str) -> String {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let limit = lines
        .iter()
        .position(|line| trim_horizontal(line).starts_with('['))
        .unwrap_or(lines.len());
    let found = lines[..limit].iter().position(|line| {
        line.contains("codex-notify.sh")
            && line
                .split_once('=')
                .is_some_and(|(key, _)| trim_horizontal(key) == "notify")
    });
    let Some(index) = found else {
        return text.to_owned();
    };
    lines.remove(index);
    lines.join("\n")
}

/// Drops a previously baked block: the marker plus `body_lines` lines below it.
fn strip_baked_block(text: &str, body_lines: usize) -> String {
    let mut skip = 0;
    let mut kept = Vec::new();
    for line in text.split('\n') {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if line == AGTERMCTL_MARKER {
            skip = body_lines;
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

#[cfg(test)]
#[path = "hooks_config_tests.rs"]
mod tests;
