//! Name and path text rules shared by the model: the control-character strip every stored name and cwd passes
//! through (upstream's `TerminalText.sanitized`), the "blank after trim" normalizer (`String.trimmedOrNil`), the
//! basename a session row falls back to (`NSString.lastPathComponent` with upstream's pins), whether a terminal title
//! may name a session instead, and the auto names of new workspaces and windows.

use std::cell::Cell;

/// Strips the C0 control range (U+0000–U+001F, tab, newline and carriage return included) and DEL (U+007F), keeping
/// every other scalar, as upstream's `TerminalText.sanitized`.
///
/// Every name and directory reaching a `{AGT_X}` custom-command token goes through it: those values flow unquoted
/// into a `/bin/sh -c` line, where a newline separates commands, and no real name or path contains a control
/// character, so the strip is lossless for real input.
#[must_use]
pub fn sanitized(value: &str) -> String {
    if !value.chars().any(is_control) {
        return value.to_owned();
    }
    value.chars().filter(|&c| !is_control(c)).collect()
}

fn is_control(c: char) -> bool {
    c < '\u{20}' || c == '\u{7f}'
}

/// The value trimmed of surrounding whitespace and newlines, `None` when nothing is left: the one normalizer for the
/// rename and display-name "blank after trim" rule (upstream's `String.trimmedOrNil`).
#[must_use]
pub fn trimmed_or_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The last path component a session row falls back to, as `NSString.lastPathComponent` with upstream's pins in
/// `Session.displayName`: a trailing slash is ignored (`/a/b/` → `b`), the root stays `/`, and an empty path reads
/// as `~`, the home shorthand, since no sensible component exists.
#[must_use]
pub fn last_path_component(path: &str) -> String {
    if path.is_empty() {
        return "~".to_owned();
    }
    let stripped = path.trim_end_matches('/');
    if stripped.is_empty() {
        return "/".to_owned();
    }
    stripped.rsplit('/').next().unwrap_or(stripped).to_owned()
}

thread_local! {
    static NAMES_FROM_TERMINAL_TITLE: Cell<bool> = const { Cell::new(false) };
}

/// Whether a pane's terminal title may stand in for an unnamed session's name (the `sessionNameFromTerminalTitle`
/// setting, off by default: the cwd basename names the session, so a TUI or agent that titles the terminal after its
/// own state cannot rename the row out from under the user). This build's setting; upstream always lets the title
/// name the session.
///
/// A mirror of the setting rather than a [`Session`](super::Session) field, because
/// [`Session::display_name`](super::Session::display_name) is read from dozens of sites that hold no settings. It is
/// per thread: the thread that owns the model (the main thread) is the only one
/// that renders a name, and parallel tests each get their own. [`Core`](crate::Core) pushes it whenever its settings
/// change; nothing else writes it outside tests.
#[must_use]
pub fn names_from_terminal_title() -> bool {
    NAMES_FROM_TERMINAL_TITLE.get()
}

/// Sets [`names_from_terminal_title`] for this thread.
pub fn set_names_from_terminal_title(on: bool) {
    NAMES_FROM_TERMINAL_TITLE.set(on);
}

/// The auto name of the `n`-th workspace (`workspace 1`, …), upstream's `AppStore.defaultWorkspaceName`.
#[must_use]
pub fn default_workspace_name(existing: usize) -> String {
    format!("workspace {}", existing + 1)
}

/// The auto name of the `n`-th window (`window 1`, …), upstream's `WindowLibrary.defaultWindowName`.
#[must_use]
pub fn default_window_name(existing: usize) -> String {
    format!("window {}", existing + 1)
}

/// Whether `name` is an auto window name: `window` and a positive integer, separated by spaces (upstream's
/// `WindowInfo.isAutoName`). The title bar shows a window's name only when it is not one.
#[must_use]
pub fn is_auto_window_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split(' ').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        ["window", number] => swift_int(number).is_some_and(|n| n >= 1),
        _ => false,
    }
}

/// Swift's `Int(String)`: an optional sign, then ASCII digits only, within 64 bits.
fn swift_int(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_strips_c0_and_del_only() {
        assert_eq!(sanitized("plain name"), "plain name");
        assert_eq!(sanitized("a\nb\tc\r\u{0}d\u{7f}e\u{1f}"), "abcde");
        assert_eq!(sanitized("ünï\u{80}cødé"), "ünï\u{80}cødé");
        assert_eq!(sanitized("\n\n"), "");
    }

    #[test]
    fn trimmed_or_none_treats_blank_as_absent() {
        assert_eq!(trimmed_or_none("  name \n"), Some("name".to_owned()));
        assert_eq!(trimmed_or_none(" \t\n "), None);
        assert_eq!(trimmed_or_none(""), None);
        assert_eq!(trimmed_or_none("\u{2003}x\u{2028}"), Some("x".to_owned()));
    }

    #[test]
    fn last_path_component_pins() {
        assert_eq!(last_path_component("/a/b"), "b");
        assert_eq!(last_path_component("/a/b/"), "b");
        assert_eq!(last_path_component("/"), "/");
        assert_eq!(last_path_component("//"), "/");
        assert_eq!(last_path_component(""), "~");
        assert_eq!(last_path_component("relative"), "relative");
        assert_eq!(last_path_component("/Users/me/my project"), "my project");
    }

    #[test]
    fn auto_window_names() {
        assert!(is_auto_window_name("window 1"));
        assert!(is_auto_window_name("window  12"));
        assert!(is_auto_window_name(" window 3 "));
        assert!(is_auto_window_name("window +2"));
        assert!(!is_auto_window_name("window 0"));
        assert!(!is_auto_window_name("window -1"));
        assert!(!is_auto_window_name("window"));
        assert!(!is_auto_window_name("window one"));
        assert!(!is_auto_window_name("Window 1"));
        assert!(!is_auto_window_name("window 1 2"));
        assert!(!is_auto_window_name("work"));
    }

    #[test]
    fn default_names_count_from_one() {
        assert_eq!(default_workspace_name(0), "workspace 1");
        assert_eq!(default_window_name(2), "window 3");
    }
}
