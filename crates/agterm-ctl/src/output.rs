//! What `agtermctl` prints for a reply and how it exits, as upstream's `SocketClient.printResponse`/`formatResponse`
//! (`agtermctlKit/SocketClient.swift`) and swift-argument-parser's reporting of an error thrown from `run()`:
//!
//! - a reply under `--json`: the server's line unchanged on stdout;
//! - a server refusal: `error: <text>` on stderr, exit 1;
//! - a local failure (connect, write, decode): `Error: <text>` on stderr, exit 1.
//!
//! Usage errors and help are [`crate::usage`]'s.

use std::process::ExitCode;

use agterm_core::theme::DEFAULT_TITLE;
use agterm_proto::keymap::{ControlHooks, ControlKeymap};
use agterm_proto::{ControlRestoreStatus, ControlTree, ControlWindowNode, Response};
use serde_json::Value;

use crate::client::Reply;
use crate::hooks::format_hooks;
use crate::keymap::format_keymap;
use crate::tree::format_tree;
use crate::window::format_windows;

/// Renders a response as its human-readable summary (no trailing newline): an `error:` line, the tree or window
/// listing, the theme listing, the keymap or hooks listing, the restore policy, the serving app's version, the text
/// read (or a search's counter), an overlay program's `exit N`, a batch's affected count, a count of diagnostics, a
/// cursor column, a resized window's `W H`, a resized split's ratio, the applied sidebar width, the affected id (only
/// with `echo_id`, for the create commands), or a bare `ok`. Never JSON: `--json` prints the server's own line.
///
/// The result's fields are tested in upstream's fixed order (`formatResponse`: `tree` first, `app` after the
/// listings); each command group adds its branch at its place in that order.
#[must_use]
pub fn format_response(response: &Response, echo_id: bool) -> String {
    if !response.ok {
        return format!("error: {}", response.error.as_deref().unwrap_or("unknown error"));
    }
    let result = response.result.as_ref();
    if let Some(listing) = result.and_then(format_listing) {
        return listing;
    }
    if let Some(app) = result.and_then(|result| result.get("app")) {
        return format_app(app);
    }
    if let Some(text) = result.and_then(|result| result.get("text")).and_then(Value::as_str) {
        return text.to_owned();
    }
    // `session.overlay.result`: the program's exit status
    if let Some(code) = result.and_then(|result| result.get("exitCode")).and_then(Value::as_i64) {
        return format!("exit {code}");
    }
    if let Some(affected) = result.and_then(|result| result.get("affected")).and_then(Value::as_i64) {
        return format_affected(affected);
    }
    // upstream reads `count` as a diagnostic count (`keymap.reload`) whatever the command, a bare search's included
    if let Some(count) = result.and_then(|result| result.get("count")).and_then(Value::as_i64) {
        return if count == 0 {
            "ok".to_owned()
        } else {
            format!("{count} diagnostic(s)")
        };
    }
    // the bare column, scriptable as a command substitution
    if let Some(column) = result
        .and_then(|result| result.get("cursor"))
        .and_then(|cursor| cursor.get("column"))
        .and_then(Value::as_i64)
    {
        return column.to_string();
    }
    // `window.resize` echoes the applied (clamped) frame size
    if let Some((width, height)) =
        result.and_then(|result| Some((result.get("width")?.as_i64()?, result.get("height")?.as_i64()?)))
    {
        return format!("{width} {height}");
    }
    // `session.resize` echoes the applied (clamped) primary-pane fraction, as `%.3f`
    if let Some(ratio) = result.and_then(|result| result.get("ratio")).and_then(Value::as_f64) {
        return format!("{ratio:.3}");
    }
    // `sidebar.width` echoes the stored (clamped) points as the Double's own description, never rounded (`300.0`,
    // `271.34`): a caller compares it against its request numerically to detect a clamp. Rust's `Debug` spelling
    // matches Swift's `description` across the drag range; they differ only in the exponent form of extreme values.
    if let Some(width) = result
        .and_then(|result| result.get("sidebarWidth"))
        .and_then(Value::as_f64)
    {
        return format!("{width:?}");
    }
    if echo_id && let Some(id) = result.and_then(|result| result.get("id")).and_then(Value::as_str) {
        return id.to_owned();
    }
    "ok".to_owned()
}

/// The listing branches of [`format_response`], in upstream's order: the tree, the windows, the themes, the keymap,
/// the hooks, the restore policy. `None` when the result carries none of them.
fn format_listing(result: &Value) -> Option<String> {
    // `client::send` already refused a reply whose tree does not decode, as upstream's whole-response decode does
    if let Some(tree) = result
        .get("tree")
        .and_then(|tree| serde_json::from_value::<ControlTree>(tree.clone()).ok())
    {
        return Some(format_tree(&tree));
    }
    // a present-but-empty list still takes this branch and renders empty
    if let Some(windows) = result
        .get("windows")
        .and_then(|windows| serde_json::from_value::<Vec<ControlWindowNode>>(windows.clone()).ok())
    {
        return Some(format_windows(&windows));
    }
    if let Some(themes) = result
        .get("themes")
        .and_then(|themes| serde_json::from_value::<Vec<String>>(themes.clone()).ok())
    {
        return Some(format_themes(&themes, result));
    }
    if let Some(keymap) = result
        .get("keymap")
        .and_then(|keymap| serde_json::from_value::<ControlKeymap>(keymap.clone()).ok())
    {
        return Some(format_keymap(&keymap));
    }
    if let Some(hooks) = result
        .get("hooks")
        .and_then(|hooks| serde_json::from_value::<ControlHooks>(hooks.clone()).ok())
    {
        return Some(format_hooks(&hooks));
    }
    if let Some(remote) = result
        .get("remote")
        .and_then(|remote| serde_json::from_value::<agterm_proto::ControlRemoteTree>(remote.clone()).ok())
    {
        return Some(crate::live::format_remote_tree(&remote));
    }
    if let Some(live) = result
        .get("live")
        .and_then(|live| serde_json::from_value::<agterm_proto::ControlLiveInventory>(live.clone()).ok())
    {
        return Some(crate::live::format_live(&live));
    }
    result
        .get("restore")
        .and_then(|restore| serde_json::from_value::<ControlRestoreStatus>(restore.clone()).ok())
        .map(|restore| format_restore_status(&restore))
}

/// `restore.mode`: what the next launch will do and what this one did, on separate lines (upstream's
/// `formatRestoreStatus`).
pub(crate) fn format_restore_status(status: &ControlRestoreStatus) -> String {
    let mut lines = vec![
        format!("configured: {} (next launch)", status.configured),
        format!(
            "this launch: requested {}, active {}",
            status.requested_at_launch, status.active
        ),
    ];
    if status.restart_required {
        lines.push("restart agterm to apply the configured mode".to_owned());
    }
    if let Some(reason) = &status.unavailable_reason {
        lines.push(format!("live unavailable: {reason}"));
    }
    lines.join("\n")
}

/// `theme.list`: one theme per line under a leading `default ghostty` entry, the active one(s) marked `* `. Plain, the
/// one `theme` is marked (none marks `default ghostty`); with `sync`, the light and dark themes are, under a header
/// naming the pair.
fn format_themes(themes: &[String], result: &Value) -> String {
    let field = |key: &str| result.get(key).and_then(Value::as_str);
    let sync = result.get("sync").and_then(Value::as_bool).unwrap_or(false);
    let (current, light, dark) = (field("theme"), field("light"), field("dark"));
    let active = |name: Option<&str>| {
        if sync {
            name.is_some() && (name == light || name == dark)
        } else {
            name == current
        }
    };
    let line = |name: Option<&str>, label: &str| format!("{}{label}", if active(name) { "* " } else { "  " });
    let body = std::iter::once(line(None, DEFAULT_TITLE))
        .chain(themes.iter().map(|name| line(Some(name), name)))
        .collect::<Vec<_>>()
        .join("\n");
    if !sync {
        return body;
    }
    format!(
        "syncing with macOS appearance — light: {}, dark: {}\n{body}",
        light.unwrap_or(DEFAULT_TITLE),
        dark.unwrap_or(DEFAULT_TITLE)
    )
}

/// `version (commit)`, or the bare version when the build recorded no commit.
fn format_app(app: &Value) -> String {
    let version = app.get("version").and_then(Value::as_str).unwrap_or_default();
    match app.get("commit").and_then(Value::as_str) {
        Some(commit) if !commit.is_empty() => format!("{version} ({commit})"),
        _ => version.to_owned(),
    }
}

/// How many sessions a batch changed: `1 session`, else `N sessions`.
fn format_affected(affected: i64) -> String {
    if affected == 1 {
        "1 session".to_owned()
    } else {
        format!("{affected} sessions")
    }
}

/// Prints a reply: the server's line with `json`, else the human summary, a refusal on stderr.
pub fn print_response(reply: &Reply, json: bool, echo_id: bool) {
    if json {
        println!("{}", reply.line());
    } else if reply.response.ok {
        println!("{}", format_response(&reply.response, echo_id));
    } else {
        eprintln!("{}", format_response(&reply.response, echo_id));
    }
}

/// Reports a local failure and exits 1.
#[must_use]
pub fn fail(message: &str) -> ExitCode {
    eprintln!("Error: {message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ok(result: Value) -> Response {
        Response::with_result(result)
    }

    #[test]
    fn version_prints_the_commit_in_parentheses() {
        let response = ok(json!({"app": {"version": "0.1.0", "commit": "abc1234"}, "conformance": {}}));
        assert_eq!(format_response(&response, false), "0.1.0 (abc1234)");
    }

    #[test]
    fn version_without_a_commit_is_bare() {
        assert_eq!(
            format_response(&ok(json!({"app": {"version": "0.1.0"}})), false),
            "0.1.0"
        );
        let empty = ok(json!({"app": {"version": "0.1.0", "commit": ""}}));
        assert_eq!(format_response(&empty, false), "0.1.0");
    }

    #[test]
    fn a_refusal_reads_error_with_a_fallback() {
        assert_eq!(
            format_response(&Response::failure("no such session: x"), false),
            "error: no such session: x"
        );
        let bare = Response {
            ok: false,
            result: None,
            error: None,
        };
        assert_eq!(format_response(&bare, false), "error: unknown error");
    }

    #[test]
    fn a_tree_wins_over_the_app() {
        let response = ok(json!({
            "tree": {"workspaces": [{"id": "W", "name": "w", "active": true, "sessions": []}]},
            "app": {"version": "0.1.0"},
        }));
        assert_eq!(format_response(&response, false), "* w  [W]");
    }

    #[test]
    fn windows_list_one_per_line_and_an_empty_list_renders_empty() {
        let response = ok(json!({"windows": [
            {"id": "w1", "name": "work", "open": true, "active": true},
            {"id": "w3", "name": "archive", "open": false, "active": false},
        ]}));
        assert_eq!(
            format_response(&response, false),
            "w1  work [open] [active]\nw3  archive"
        );
        assert_eq!(format_response(&ok(json!({"windows": []})), true), "");
    }

    fn theme_lines(result: Value) -> Vec<String> {
        format_response(&ok(result), false)
            .split('\n')
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn themes_mark_the_current_one() {
        assert_eq!(
            theme_lines(json!({"theme": "Nord", "themes": ["Dracula", "Nord"]})),
            ["  default ghostty", "  Dracula", "* Nord"]
        );
    }

    #[test]
    fn themes_mark_the_ghostty_default_when_no_theme_is_set() {
        assert_eq!(
            theme_lines(json!({"themes": ["Dracula"]})),
            ["* default ghostty", "  Dracula"]
        );
        assert_eq!(theme_lines(json!({"themes": []})), ["* default ghostty"]);
    }

    #[test]
    fn a_theme_set_without_a_list_is_bare_ok() {
        assert_eq!(format_response(&ok(json!({"theme": "Dracula"})), false), "ok");
    }

    #[test]
    fn syncing_themes_mark_both_sides_under_a_header() {
        let lines = theme_lines(json!({
            "themes": ["agterm", "Builtin Light", "Nord"], "sync": true, "light": "Builtin Light", "dark": "agterm",
        }));
        assert_eq!(
            lines,
            [
                "syncing with macOS appearance — light: Builtin Light, dark: agterm",
                "  default ghostty",
                "* agterm",
                "* Builtin Light",
                "  Nord",
            ]
        );
        // a missing side names the built-in default, which is never marked while syncing
        assert_eq!(
            theme_lines(json!({"themes": ["Nord"], "sync": true, "dark": "Nord"})),
            [
                "syncing with macOS appearance — light: default ghostty, dark: Nord",
                "  default ghostty",
                "* Nord",
            ]
        );
        // an empty side is kept, as Swift's `??`
        assert_eq!(
            theme_lines(json!({"themes": [], "sync": true, "light": "", "dark": "Nord"}))[0],
            "syncing with macOS appearance — light: , dark: Nord"
        );
    }

    #[test]
    fn themes_sit_after_the_windows_and_before_the_app() {
        assert_eq!(
            format_response(&ok(json!({"windows": [], "themes": ["Nord"]})), false),
            ""
        );
        assert_eq!(
            format_response(&ok(json!({"themes": [], "app": {"version": "1"}, "text": "t"})), false),
            "* default ghostty"
        );
    }

    #[test]
    fn a_batch_reports_how_many_sessions_it_changed() {
        for (affected, expected) in [(1, "1 session"), (2, "2 sessions"), (0, "0 sessions")] {
            assert_eq!(format_response(&ok(json!({"affected": affected})), false), expected);
        }
        // the count wins over an echoed id
        assert_eq!(
            format_response(&ok(json!({"id": "abc", "affected": 2})), true),
            "2 sessions"
        );
    }

    #[test]
    fn a_resize_prints_the_applied_width_and_height() {
        assert_eq!(
            format_response(&ok(json!({"width": 900, "height": 640})), false),
            "900 640"
        );
        // the size wins over an echoed id, and needs both halves
        assert_eq!(
            format_response(&ok(json!({"id": "abc", "width": 900, "height": 640})), true),
            "900 640"
        );
        assert_eq!(format_response(&ok(json!({"width": 900})), false), "ok");
    }

    #[test]
    fn text_prints_as_is_and_wins_over_a_count() {
        assert_eq!(
            format_response(&ok(json!({"text": "line 1\nline 2\n"})), false),
            "line 1\nline 2\n"
        );
        assert_eq!(format_response(&ok(json!({"text": ""})), false), "");
        assert_eq!(
            format_response(&ok(json!({"id": "S", "text": "2/5", "count": 5, "pane": "left"})), true),
            "2/5"
        );
        // the app wins over the text, as upstream tests it first
        assert_eq!(
            format_response(&ok(json!({"app": {"version": "0.1.0"}, "text": "x"})), false),
            "0.1.0"
        );
    }

    #[test]
    fn keymap_and_hooks_listings_sit_after_the_themes_and_before_the_app() {
        let keymap = json!({"path": "/k", "actions": [], "commands": [], "diagnostics": []});
        let hooks = json!({"path": "/h", "diagnostics": [], "hooks": []});
        assert_eq!(
            format_response(
                &ok(json!({"keymap": keymap, "app": {"version": "1"}, "count": 2})),
                false
            ),
            "keymap: /k\n\nactions:"
        );
        assert_eq!(
            format_response(&ok(json!({"hooks": hooks, "text": "t"})), false),
            "hooks: /h\n  (no hooks)"
        );
        assert_eq!(
            format_response(&ok(json!({"keymap": keymap, "hooks": hooks})), false),
            "keymap: /k\n\nactions:"
        );
        assert_eq!(
            format_response(&ok(json!({"themes": [], "keymap": keymap})), false),
            "* default ghostty"
        );
    }

    fn restore_status(restart_required: bool, reason: Option<&str>) -> Value {
        json!({
            "configured": "live",
            "requestedAtLaunch": if restart_required { "rerun" } else { "live" },
            "active": if reason.is_some() { "none" } else if restart_required { "rerun" } else { "live" },
            "restartRequired": restart_required,
            "unavailableReason": reason,
        })
    }

    #[test]
    fn the_restore_status_separates_the_next_launch_from_this_one() {
        assert_eq!(
            format_response(&ok(json!({"restore": restore_status(false, None)})), false),
            "configured: live (next launch)\nthis launch: requested live, active live"
        );
        assert_eq!(
            format_response(&ok(json!({"restore": restore_status(true, None)})), false),
            "configured: live (next launch)\nthis launch: requested rerun, active rerun\nrestart agterm to apply the \
             configured mode"
        );
        assert_eq!(
            format_response(
                &ok(json!({"restore": restore_status(false, Some("no live-session executable"))})),
                false
            ),
            "configured: live (next launch)\nthis launch: requested live, active none\nlive unavailable: no live-session executable"
        );
        assert_eq!(
            format_response(
                &ok(json!({"restore": restore_status(true, Some("no live-session executable"))})),
                false
            ),
            "configured: live (next launch)\nthis launch: requested rerun, active none\nrestart agterm to apply the \
             configured mode\nlive unavailable: no live-session executable"
        );
    }

    #[test]
    fn the_restore_status_sits_after_the_hooks_and_before_the_app() {
        let hooks = json!({"path": "/h", "diagnostics": [], "hooks": []});
        let restore = restore_status(false, None);
        assert_eq!(
            format_response(&ok(json!({"hooks": hooks, "restore": restore})), false),
            "hooks: /h\n  (no hooks)"
        );
        assert_eq!(
            format_response(
                &ok(json!({"restore": restore, "app": {"version": "1"}, "text": "t", "count": 2})),
                false
            ),
            "configured: live (next launch)\nthis launch: requested live, active live"
        );
        // a malformed status falls through, as an undecodable listing does
        assert_eq!(
            format_response(&ok(json!({"restore": {"configured": "live"}})), false),
            "ok"
        );
    }

    #[test]
    fn a_capture_prints_its_text_rather_than_a_diagnostic_count() {
        assert_eq!(
            format_response(&ok(json!({"count": 2, "text": "captured 2 panes"})), false),
            "captured 2 panes"
        );
    }

    #[test]
    fn a_count_reads_as_diagnostics_and_zero_as_ok() {
        assert_eq!(format_response(&ok(json!({"count": 0})), false), "ok");
        assert_eq!(format_response(&ok(json!({"count": 3})), false), "3 diagnostic(s)");
        assert_eq!(
            format_response(&ok(json!({"affected": 2, "count": 3})), false),
            "2 sessions"
        );
    }

    #[test]
    fn a_cursor_prints_its_column_alone() {
        let response = ok(json!({"id": "surface:S:left", "cursor": {"column": 12}}));
        assert_eq!(format_response(&response, false), "12");
        assert_eq!(format_response(&ok(json!({"cursor": {"column": 0}})), true), "0");
        // the column wins over a size, as upstream tests it first
        assert_eq!(
            format_response(&ok(json!({"cursor": {"column": 4}, "width": 1, "height": 2})), false),
            "4"
        );
    }

    #[test]
    fn a_split_resize_prints_the_applied_ratio_to_three_places() {
        assert_eq!(
            format_response(&ok(json!({"id": "9f3c", "ratio": 0.85})), false),
            "0.850"
        );
        assert_eq!(format_response(&ok(json!({"id": "9f3c", "ratio": 0.3})), true), "0.300");
        assert_eq!(format_response(&ok(json!({"ratio": 1})), false), "1.000");
        // the size wins over a ratio, as upstream tests it first
        assert_eq!(
            format_response(&ok(json!({"width": 1, "height": 2, "ratio": 0.5})), false),
            "1 2"
        );
    }

    #[test]
    fn the_applied_sidebar_width_prints_unrounded_after_the_ratio() {
        for (stored, rendered) in [(271.3, "271.3"), (271.34, "271.34"), (300.0, "300.0")] {
            assert_eq!(format_response(&ok(json!({"sidebarWidth": stored})), false), rendered);
        }
        // an integral number on the wire is still a Double
        assert_eq!(format_response(&ok(json!({"sidebarWidth": 300})), false), "300.0");
        // the fields upstream reads first still win, and the width wins over an echoed id
        assert_eq!(
            format_response(&ok(json!({"text": "t", "sidebarWidth": 1.0})), false),
            "t"
        );
        assert_eq!(
            format_response(&ok(json!({"ratio": 0.5, "sidebarWidth": 1.0})), false),
            "0.500"
        );
        assert_eq!(
            format_response(&ok(json!({"id": "W", "sidebarWidth": 1.0})), true),
            "1.0"
        );
    }

    #[test]
    fn an_overlay_result_prints_its_exit_status_after_the_text() {
        assert_eq!(format_response(&ok(json!({"id": "S", "exitCode": 5})), true), "exit 5");
        assert_eq!(format_response(&ok(json!({"exitCode": 0})), false), "exit 0");
        // the text wins over the status and the status over a count, as upstream tests them in that order
        assert_eq!(format_response(&ok(json!({"text": "t", "exitCode": 1})), false), "t");
        assert_eq!(
            format_response(&ok(json!({"exitCode": 2, "affected": 3})), false),
            "exit 2"
        );
    }

    #[test]
    fn the_id_is_echoed_only_when_asked() {
        let response = ok(json!({"id": "abc"}));
        assert_eq!(format_response(&response, true), "abc");
        assert_eq!(format_response(&response, false), "ok");
        assert_eq!(format_response(&Response::success(), true), "ok");
    }
}
