//! `hooks.conf`: shell lines run on control events (upstream's `Hooks.swift`, `HookScheduler.swift`, the hooks half of
//! `ConfigPaths.swift` and `SettingsModel`, and the host-free part of `HookProcessRunner`).
//!
//! The file holds `on <kind> <shell...>` lines; [`parse_hooks_conf`] turns it into [`Hooks`] plus diagnostics, and
//! [`HookScheduler`] runs each hook against the control events, one child per hook at a time behind an ordered bounded
//! queue. The frontend owns the processes: it implements [`HookLauncher`], feeds the child [`hook_environment`] and
//! [`hook_stdin`], and reports the exit back through [`HookScheduler::exited`].

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ngterm_proto::{ControlEvent, EventKind};
use strum::IntoEnumIterator;

use crate::keymap::KeymapDiagnostic;

pub mod scheduler;

pub use scheduler::{HookLauncher, HookRun, HookRunToken, HookScheduler, PENDING_CAPACITY};

/// What makes two `hooks.conf` lines the same hook: the event kind and the shell line, outer whitespace trimmed. The
/// line number is deliberately not part of it, so inserting a comment above a hook keeps its running child and queue
/// across a reload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HookIdentity {
    pub kind: EventKind,
    pub command: String,
}

/// One `on <kind> <shell...>` line. `line` (1-based) is display data for diagnostics and `hooks.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEntry {
    pub identity: HookIdentity,
    pub line: usize,
}

impl HookEntry {
    /// The event kind the hook runs on.
    #[must_use]
    pub fn kind(&self) -> EventKind {
        self.identity.kind
    }

    /// The shell line, passed to `/bin/sh -c` untouched.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.identity.command
    }
}

/// The parsed `hooks.conf`, entries in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hooks {
    pub entries: Vec<HookEntry>,
}

/// The `hooks.conf` inside a config directory: `<dir>/hooks.conf`, beside `keymap.conf` (`ConfigPaths.hooksPath`).
#[must_use]
pub fn hooks_path(config_dir: &Path) -> PathBuf {
    config_dir.join("hooks.conf")
}

/// Parses `hooks.conf` (upstream's `parseHooksConf`). Blank lines and whole-line `#` comments are skipped; everything
/// after the kind is the shell line verbatim, so an inline `#`, quotes, pipes and `$VAR` reach `/bin/sh` untouched. A
/// malformed line or a duplicate kind+command is diagnosed and skipped without stopping later lines.
#[must_use]
pub fn parse_hooks_conf(text: &str) -> (Hooks, Vec<KeymapDiagnostic>) {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut diagnostics = Vec::new();

    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for (index, raw_line) in normalized.split('\n').enumerate() {
        let line_number = index + 1;
        let line = trim_whitespaces(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match parse_line(line) {
            Ok(identity) if seen.contains(&identity) => {
                let message = format!(
                    "hook 'on {} {}' is already defined; hook skipped",
                    identity.kind, identity.command
                );
                diagnostics.push(KeymapDiagnostic::new(line_number, message));
            }
            Ok(identity) => {
                seen.insert(identity.clone());
                entries.push(HookEntry {
                    identity,
                    line: line_number,
                });
            }
            Err(message) => diagnostics.push(KeymapDiagnostic::new(line_number, message)),
        }
    }
    (Hooks { entries }, diagnostics)
}

/// One trimmed, non-comment line as a hook identity, or the diagnostic's message.
fn parse_line(line: &str) -> Result<HookIdentity, String> {
    let verb = leading_word(line);
    if verb != "on" {
        return Err(format!("unknown verb '{verb}'"));
    }
    let rest = trim_whitespaces(&line[verb.len()..]);
    let raw_kind = leading_word(rest);
    if raw_kind.is_empty() {
        return Err("on requires an event kind".to_owned());
    }
    let kind = raw_kind
        .parse::<EventKind>()
        .map_err(|_| format!("unknown event kind '{raw_kind}'"))?;
    let command = trim_whitespaces(&rest[raw_kind.len()..]);
    if command.is_empty() {
        return Err(format!("hook for '{raw_kind}' has no shell line"));
    }
    Ok(HookIdentity {
        kind,
        command: command.to_owned(),
    })
}

/// The text up to the first whitespace character (Swift's `prefix(while: { !$0.isWhitespace })`).
fn leading_word(text: &str) -> &str {
    text.split(char::is_whitespace).next().unwrap_or_default()
}

/// Trims Foundation's `CharacterSet.whitespaces`: tab and the space separators (`Zs`), never a line or paragraph
/// break, a vertical tab or a form feed.
fn trim_whitespaces(text: &str) -> &str {
    text.trim_matches(|c: char| {
        c.is_whitespace()
            && !matches!(
                c,
                '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
            )
    })
}

/// Reads and parses `hooks.conf` at `path` (upstream's `SettingsModel.loadHooks`). A missing file means no hooks and
/// no diagnostics; a file that exists but cannot be read must not read as clean, or a reload would silently retire
/// every hook, so it yields no hooks and one line-0 diagnostic.
#[must_use]
pub fn load_hooks(path: &Path) -> (Hooks, Vec<KeymapDiagnostic>) {
    match fs::read_to_string(path) {
        Ok(text) => parse_hooks_conf(&text),
        Err(_) if !path.exists() => (Hooks::default(), Vec::new()),
        Err(err) => {
            let message = format!("could not read hooks.conf: {}", read_error_reason(&err));
            (Hooks::default(), vec![KeymapDiagnostic::new(0, message)])
        }
    }
}

/// A readable reason for a failed read: the OS error's text without its ` (os error N)` suffix, or Foundation's
/// wording for a file that is not UTF-8. Shared with `keymap.conf`'s loader.
pub(crate) fn read_error_reason(err: &io::Error) -> String {
    if err.kind() == io::ErrorKind::InvalidData {
        return "the file isn't valid UTF-8 text".to_owned();
    }
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(index) => text[..index].to_owned(),
        None => text,
    }
}

/// The commented starter `hooks.conf` (upstream's `ConfigPaths.starterHooksConf`): the syntax, every event kind, the
/// delivery contract and a few examples. Every line is a comment so a fresh file runs nothing. The one change from
/// upstream's text is the uncommentable example, which plays a sound with `canberra-gtk-play` instead of macOS's
/// `afplay`.
#[must_use]
pub fn starter_hooks_conf() -> String {
    let kinds = EventKind::iter()
        .map(EventKind::wire_name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\
# ngterm hooks — run a shell line when a control event fires. Edit this file and run
# File ▸ Reload Hooks (or `ngtermctl hooks reload`) to apply. Blank lines and lines starting
# with `#` are ignored.
#
#   on <kind> <shell...>
#
# Kinds: {kinds}.
# Several lines per kind are allowed and run independently; an identical kind+command line is
# skipped with a diagnostic. Everything after the kind is the shell line, passed to `/bin/sh -c`
# untouched, detached with no terminal, in the app's working directory, one process per line at
# a time. Further events for a busy line queue in order (256 pending, oldest dropped).
#
# The script gets the event as one JSON object on stdin, the shape `ngtermctl events --json` prints,
# and these variables: AGT_EVENT_KIND, AGT_EVENT_STATUS, AGT_EVENT_HOST, AGT_SESSION_ID,
# AGT_WORKSPACE_ID, AGT_WINDOW_ID, AGT_SOCKET. Pass `--socket \"$AGT_SOCKET\"` to any ngtermctl
# call. A hook whose command emits another event of its own kind triggers itself again; the
# queue bounds concurrency, it does not detect loops.
#
# Examples:
#
#   on status           ~/.config/ngterm/hooks/status.sh
#   on session.closed   echo \"$(date +%T) closed $AGT_SESSION_ID\" >> ~/.local/state/ngterm/sessions.log
#   (a redirection does not create its directory: mkdir -p ~/.local/state/ngterm first)
#   on pane.scratch     ~/.config/ngterm/hooks/scratch-layout.sh
#   on tree.changed     ~/.config/ngterm/hooks/sync-outline.sh
#
# Uncomment and edit a line below to start.
# on status [ \"$AGT_EVENT_STATUS\" = blocked ] || exit 0; canberra-gtk-play -i dialog-warning
"
    )
}

/// Writes [`starter_hooks_conf`] (and its directory) to `path` when no file exists there (upstream's
/// `SettingsModel.ensureStarterHooks`). Also the Edit Hooks entry point: the file can be missing after a
/// config-directory change or a manual delete. A failure is logged and otherwise ignored.
pub fn ensure_starter_hooks(path: &Path) {
    if path.exists() {
        return;
    }
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| crate::persist::json::write_atomic(path, starter_hooks_conf().as_bytes()));
    if let Err(err) = written {
        tracing::warn!(%err, path = %path.display(), "could not write the starter hooks");
    }
}

/// The variables a hook child gets on top of the app's environment (upstream's `HookProcessRunner.environment`), in a
/// fixed order. Every one is set, empty when the event lacks the field, so an inherited value can never point a script
/// at the wrong session. `socket` is the app's control socket path.
#[must_use]
pub fn hook_environment(event: &ControlEvent, socket: &str) -> Vec<(String, String)> {
    let field = |value: Option<&String>| value.cloned().unwrap_or_default();
    vec![
        ("AGT_EVENT_KIND".to_owned(), event.kind.wire_name().to_owned()),
        ("AGT_EVENT_STATUS".to_owned(), field(event.payload.status.as_ref())),
        ("AGT_EVENT_HOST".to_owned(), field(event.payload.host.as_ref())),
        ("AGT_SESSION_ID".to_owned(), field(event.session.as_ref())),
        ("AGT_WORKSPACE_ID".to_owned(), field(event.workspace.as_ref())),
        ("AGT_WINDOW_ID".to_owned(), field(event.window.as_ref())),
        ("AGT_SOCKET".to_owned(), socket.to_owned()),
    ]
}

/// What a hook child reads on stdin: the event as one compact JSON object, the shape `events.read` returns and
/// `ngtermctl events --json` prints, then a newline.
///
/// # Errors
///
/// The event could not be encoded; the frontend reports it as `encode: <error>` through
/// [`HookScheduler::delivery_failed`], as upstream's runner does.
pub fn hook_stdin(event: &ControlEvent) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(event)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests;
