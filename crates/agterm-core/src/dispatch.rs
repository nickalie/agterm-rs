//! The core state owner and the command dispatcher: one request in, one response out, on the main thread.

use agterm_proto::{AppIdentity, Command, ConformanceProfile, PARITY_LEVEL, Request, Response, VersionResult};
use serde::Serialize;
use serde_json::Value;

use crate::error::ControlError;

/// What the app tells the core about itself at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreConfig {
    /// The serving build, reported by `version` and `tree.app`.
    pub app: AppIdentity,
    /// The conformance capabilities this host claims (`conformance/docs/rust-build.md`).
    pub capabilities: Vec<String>,
}

/// The single owner of the app state. Lives on the main thread; every control request reaches it through
/// [`Core::dispatch`].
#[derive(Debug)]
pub struct Core {
    app: AppIdentity,
    capabilities: Vec<String>,
}

/// Foundation's context for data that does not parse as JSON (swift-foundation's `JSONDecoder.decode`), whatever the
/// syntax error; upstream's socket answers it for a line that is not JSON at all.
const NOT_JSON: &str = "The given data was not valid JSON.";

/// Decodes one request line (newline excluded), as upstream's `handleConnection` decodes a `ControlRequest`
/// (`agterm/Control/ControlServer.swift`).
///
/// # Errors
///
/// [`ControlError::InvalidRequest`] for anything that is not a well-typed request object naming a known command. Its
/// context is the decoder's: Foundation's own sentence for a line that is not JSON and for an unknown `cmd`, serde's
/// message for the rest (a missing `cmd`, a value of the wrong type, a non-object). An unknown `cmd` wins over every
/// other defect, as Foundation decodes `cmd` first; that keeps the command an old app does not know named in the
/// error, whatever else the newer client sent.
pub fn decode_request(line: &[u8]) -> Result<Request, ControlError> {
    let value: Value = serde_json::from_slice(line).map_err(|_| ControlError::InvalidRequest(NOT_JSON.to_owned()))?;
    if let Some(Value::String(cmd)) = value.get("cmd")
        && let Err(unknown) = cmd.parse::<Command>()
    {
        return Err(ControlError::InvalidRequest(unknown.to_string()));
    }
    // from a parsed value, serde's messages carry no line and column, as Foundation's context carries none
    serde_json::from_value(value).map_err(|error| ControlError::InvalidRequest(error.to_string()))
}

fn result_of(value: &impl Serialize) -> Result<Response, ControlError> {
    serde_json::to_value(value)
        .map(Response::with_result)
        .map_err(|_| ControlError::Encode)
}

impl Core {
    #[must_use]
    pub fn new(config: CoreConfig) -> Self {
        Self {
            app: config.app,
            capabilities: config.capabilities,
        }
    }

    /// Answers one request. Failures become `ok:false` with the error's text.
    pub fn dispatch(&mut self, request: &Request) -> Response {
        self.route(request).unwrap_or_else(Response::from)
    }

    /// Decodes and answers one request line, as the socket receives it.
    pub fn dispatch_line(&mut self, line: &[u8]) -> Response {
        match decode_request(line) {
            Ok(request) => self.dispatch(&request),
            Err(error) => error.into(),
        }
    }

    /// The `version` result.
    #[must_use]
    pub fn version(&self) -> VersionResult {
        VersionResult {
            app: self.app.clone(),
            conformance: ConformanceProfile {
                parity_level: PARITY_LEVEL.to_owned(),
                capabilities: self.capabilities.clone(),
            },
        }
    }

    /// One arm per group, routing to the group's handler; groups without one yet are refused by name.
    fn route(&mut self, request: &Request) -> Result<Response, ControlError> {
        match request.cmd {
            Command::Version => result_of(&self.version()),
            cmd @ (Command::Ask(_)
            | Command::Browser(_)
            | Command::Config(_)
            | Command::Dashboard
            | Command::Debug(_)
            | Command::Events(_)
            | Command::Font(_)
            | Command::Hooks(_)
            | Command::Keymap(_)
            | Command::Notify
            | Command::Pick(_)
            | Command::Quick(_)
            | Command::Restore(_)
            | Command::Session(_)
            | Command::Sidebar(_)
            | Command::Surface(_)
            | Command::Theme(_)
            | Command::Tree
            | Command::Window(_)
            | Command::Workspace(_)
            | Command::Zmx(_)) => Err(ControlError::NotSupported(cmd)),
        }
    }
}

#[cfg(test)]
mod tests {
    use agterm_proto::command::{SessionCmd, ZmxCmd};
    use serde_json::json;

    use super::*;

    fn core() -> Core {
        Core::new(CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
            capabilities: vec!["posix-shell".to_owned()],
        })
    }

    #[test]
    fn version_reports_app_and_profile() {
        let response = core().dispatch(&Request::new(Command::Version));
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({"ok": true, "result": {
                "app": {"version": "0.1.0", "commit": "abc1234"},
                "conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell"]}
            }})
        );
    }

    #[test]
    fn unimplemented_commands_are_refused_by_name() {
        let mut core = core();
        for cmd in [
            Command::Tree,
            Command::Session(SessionCmd::New),
            Command::Zmx(ZmxCmd::List),
        ] {
            let response = core.dispatch(&Request::new(cmd));
            assert!(!response.ok);
            assert_eq!(
                response.error,
                Some(format!("{} is not supported by this host", cmd.wire_name()))
            );
        }
    }

    #[test]
    fn every_command_gets_an_answer() {
        let mut core = core();
        for cmd in Command::all() {
            let response = core.dispatch(&Request::new(cmd));
            assert_eq!(response.ok, cmd == Command::Version, "{cmd}");
        }
    }

    /// The cases of `test_undecodable_request_is_refused` (`conformance/tests/protocol/test_addressing.py`), with the
    /// full text the suite leaves open.
    #[test]
    fn undecodable_requests_answer_invalid_request_with_the_decoder_context() {
        let mut core = core();
        for (line, error) in [
            (
                &br#"{"cmd":"no.such.command"}"#[..],
                "invalid request: Cannot initialize Command from invalid String value no.such.command",
            ),
            (b"not json", "invalid request: The given data was not valid JSON."),
            (br#"{"target":"active"}"#, "invalid request: missing field `cmd`"),
            (
                b"[]",
                "invalid request: invalid type: sequence, expected a JSON object (Request)",
            ),
            (
                br#"{"cmd":"version","target":5}"#,
                "invalid request: invalid type: integer `5`, expected a string",
            ),
            (
                br#"{"cmd":"events.read","args":{"limit":"5"}}"#,
                r#"invalid request: invalid type: string "5", expected i64"#,
            ),
        ] {
            assert_eq!(core.dispatch_line(line), Response::failure(error), "{line:?}");
        }
    }

    #[test]
    fn other_decode_failures_carry_their_context() {
        let mut core = core();
        for (line, error) in [
            (&b""[..], "invalid request: The given data was not valid JSON."),
            (
                br#"{"cmd":"version"} trailing"#,
                "invalid request: The given data was not valid JSON.",
            ),
            (
                br#"{"cmd":5}"#,
                "invalid request: invalid type: integer `5`, expected a string",
            ),
            (
                br#"{"cmd":"version","args":["x"]}"#,
                "invalid request: invalid type: sequence, expected a JSON object (Args)",
            ),
            (
                br#"{"cmd":"pick.open","args":{"items":[{"id":"1"}]}}"#,
                "invalid request: missing field `label`",
            ),
            // Foundation decodes `cmd` first, so an unknown one is named even next to another defect
            (
                br#"{"args":{"limit":"5"},"cmd":"session.nope","target":5}"#,
                "invalid request: Cannot initialize Command from invalid String value session.nope",
            ),
        ] {
            assert_eq!(core.dispatch_line(line), Response::failure(error), "{line:?}");
        }
        assert!(core.dispatch_line(br#"{"cmd":"version","args":{"bogus":1}}"#).ok);
    }
}
