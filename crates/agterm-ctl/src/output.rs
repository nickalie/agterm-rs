//! What `agtermctl` prints and how it exits, as upstream's `SocketClient.printResponse`/`formatResponse` and
//! swift-argument-parser's error reporting:
//!
//! - a reply under `--json`: the server's line unchanged on stdout;
//! - a server refusal: `error: <text>` on stderr, exit 1;
//! - a local failure (connect, write, decode): `Error: <text>` on stderr, exit 1;
//! - a usage error: `Error: <text>`, the usage line and a help hint on stderr, exit 64 (`EX_USAGE`);
//! - help, asked for or shown for a bare group: stdout, exit 0.

use std::process::ExitCode;

use agterm_proto::Response;
use serde_json::Value;

use crate::client::Reply;

/// `EX_USAGE`: the exit status of an argument error, as swift-argument-parser's `ValidationError`.
pub const USAGE_EXIT: u8 = 64;

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

/// A clap parse outcome rendered upstream's way: what to print, where, and the exit status.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseExit {
    pub text: String,
    pub to_stdout: bool,
    pub code: u8,
}

/// Maps a clap error to upstream's report. Help (asked for, or shown because a command needs a subcommand) goes to
/// stdout with exit 0; anything else becomes `Error: <message>`, clap's usage line and a `See '<command> --help'`
/// hint on stderr with exit 64.
#[must_use]
pub fn parse_exit(error: &clap::Error) -> ParseExit {
    use clap::error::ErrorKind;
    let rendered = error.render().to_string();
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        return ParseExit {
            text: rendered,
            to_stdout: true,
            code: 0,
        };
    }
    ParseExit {
        text: usage_report(&rendered),
        to_stdout: false,
        code: USAGE_EXIT,
    }
}

/// Rebuilds clap's `error: ...` report as `Error: <Message>\nUsage: ...\n  See '<command> --help' for more
/// information.\n`. The message is every line before the usage line, so the missing arguments clap lists under
/// `the following required arguments were not provided:` and an invalid value's `[possible values: ...]` stay;
/// blank lines, clap's `tip:` lines and its `For more information, try '--help'.` (a report without a usage line ends
/// with it) go.
fn usage_report(rendered: &str) -> String {
    let message: Vec<&str> = rendered
        .lines()
        .take_while(|line| !line.starts_with("Usage: ") && !line.starts_with("For more information, try "))
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with("tip:"))
        .collect();
    let message = message.join("\n");
    let message = capitalized(message.strip_prefix("error: ").unwrap_or(&message));
    let usage = rendered
        .lines()
        .find(|line| line.starts_with("Usage: "))
        .unwrap_or("Usage: agtermctl <COMMAND>");
    let command: Vec<&str> = usage
        .trim_start_matches("Usage: ")
        .split_whitespace()
        .take_while(|word| !word.starts_with(['[', '<', '-']))
        .collect();
    format!(
        "Error: {message}\n{usage}\n  See '{} --help' for more information.\n",
        command.join(" ")
    )
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
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
    fn the_id_is_echoed_only_when_asked() {
        let response = ok(json!({"id": "abc"}));
        assert_eq!(format_response(&response, true), "abc");
        assert_eq!(format_response(&response, false), "ok");
        assert_eq!(format_response(&Response::success(), true), "ok");
    }

    #[test]
    fn a_clap_error_is_reported_as_upstream() {
        let rendered = "error: unexpected argument '--bogus' found\n\n  tip: to pass '--bogus' as a value, use \
                        '-- --bogus'\n\nUsage: agtermctl version [OPTIONS]\n\nFor more information, try '--help'.\n";
        assert_eq!(
            usage_report(rendered),
            "Error: Unexpected argument '--bogus' found\nUsage: agtermctl version [OPTIONS]\n  See 'agtermctl \
             version --help' for more information.\n"
        );
    }

    /// A command with a required argument and an enumerated one, as later command groups have.
    fn parse_error(args: &[&str]) -> ParseExit {
        let command = clap::Command::new("agtermctl").subcommand(
            clap::Command::new("pick")
                .arg(clap::Arg::new("target").required(true))
                .arg(clap::Arg::new("mode").long("mode").value_parser(["fast", "slow"])),
        );
        parse_exit(&command.try_get_matches_from(args).unwrap_err())
    }

    #[test]
    fn a_missing_argument_is_named() {
        let report = parse_error(&["agtermctl", "pick"]);
        assert_eq!(report.code, USAGE_EXIT);
        assert_eq!(
            report.text,
            "Error: The following required arguments were not provided:\n  <target>\nUsage: agtermctl pick \
             <target>\n  See 'agtermctl pick --help' for more information.\n"
        );
    }

    #[test]
    fn an_invalid_value_keeps_its_choices() {
        let report = parse_error(&["agtermctl", "pick", "x", "--mode", "warp"]);
        let message = report.text.lines().take_while(|line| !line.starts_with("Usage: "));
        assert_eq!(
            message.collect::<Vec<_>>(),
            [
                "Error: Invalid value 'warp' for '--mode <mode>'",
                "  [possible values: fast, slow]"
            ]
        );
    }

    #[test]
    fn a_report_without_usage_still_names_the_root() {
        assert_eq!(
            usage_report("error: boom\n"),
            "Error: Boom\nUsage: agtermctl <COMMAND>\n  See 'agtermctl --help' for more information.\n"
        );
    }
}
