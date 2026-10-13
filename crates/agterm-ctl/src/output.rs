//! What `agtermctl` prints for a reply and how it exits, as upstream's `SocketClient.printResponse`/`formatResponse`
//! (`agtermctlKit/SocketClient.swift`) and swift-argument-parser's reporting of an error thrown from `run()`:
//!
//! - a reply under `--json`: the server's line unchanged on stdout;
//! - a server refusal: `error: <text>` on stderr, exit 1;
//! - a local failure (connect, write, decode): `Error: <text>` on stderr, exit 1.
//!
//! Usage errors and help are [`crate::usage`]'s.

use std::process::ExitCode;

use agterm_proto::{ControlTree, Response};
use serde_json::Value;

use crate::client::Reply;
use crate::tree::format_tree;

/// Renders a response as its human-readable summary (no trailing newline): an `error:` line, the serving app's
/// version, the affected id (only with `echo_id`, for the create commands), or a bare `ok`. Never JSON: `--json`
/// prints the server's own line.
///
/// Upstream tests the result's fields in a fixed order (`tree` before `app`, since a tree carries `app` too); each
/// command group adds its branch at its place in that order.
#[must_use]
pub fn format_response(response: &Response, echo_id: bool) -> String {
    if !response.ok {
        return format!("error: {}", response.error.as_deref().unwrap_or("unknown error"));
    }
    let result = response.result.as_ref();
    // a tree this build cannot decode reads as no tree, as a field it does not model would
    if let Some(tree) = result
        .and_then(|result| result.get("tree"))
        .and_then(|tree| serde_json::from_value::<ControlTree>(tree.clone()).ok())
    {
        return format_tree(&tree);
    }
    if let Some(app) = result.and_then(|result| result.get("app")) {
        return format_app(app);
    }
    if echo_id && let Some(id) = result.and_then(|result| result.get("id")).and_then(Value::as_str) {
        return id.to_owned();
    }
    "ok".to_owned()
}

/// `version (commit)`, or the bare version when the build recorded no commit.
fn format_app(app: &Value) -> String {
    let version = app.get("version").and_then(Value::as_str).unwrap_or_default();
    match app.get("commit").and_then(Value::as_str) {
        Some(commit) if !commit.is_empty() => format!("{version} ({commit})"),
        _ => version.to_owned(),
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
    fn a_tree_wins_over_the_app_it_carries() {
        let response = ok(
            json!({"tree": {"workspaces": [{"id": "W", "name": "w", "active": true, "sessions": []}],
                                          "app": {"version": "0.1.0"}}}),
        );
        assert_eq!(format_response(&response, false), "* w  [W]");
        let undecodable = ok(json!({"tree": {"workspaces": 5}}));
        assert_eq!(format_response(&undecodable, false), "ok");
    }

    #[test]
    fn the_id_is_echoed_only_when_asked() {
        let response = ok(json!({"id": "abc"}));
        assert_eq!(format_response(&response, true), "abc");
        assert_eq!(format_response(&response, false), "ok");
        assert_eq!(format_response(&Response::success(), true), "ok");
    }
}
