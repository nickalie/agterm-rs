//! The core state owner and the command dispatcher: one request in, one response out, on the main thread.

use agterm_proto::{AppIdentity, Command, ConformanceProfile, PARITY_LEVEL, Request, Response, VersionResult};
use serde::Serialize;

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

/// Decodes one request line (newline excluded).
///
/// # Errors
///
/// [`ControlError::MalformedRequest`] for anything that is not a well-typed request object naming a known command.
pub fn decode_request(line: &[u8]) -> Result<Request, ControlError> {
    serde_json::from_slice(line).map_err(|_| ControlError::MalformedRequest)
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

    #[test]
    fn undecodable_lines_are_malformed_requests() {
        let mut core = core();
        for line in [
            &b"not json"[..],
            b"",
            b"[]",
            br#"{"cmd":"no.such.command"}"#,
            br#"{"target":"active"}"#,
            br#"{"cmd":"version","target":5}"#,
            br#"{"cmd":"events.read","args":{"limit":"5"}}"#,
        ] {
            let response = core.dispatch_line(line);
            assert_eq!(response, Response::failure("could not decode request"), "{line:?}");
        }
        assert!(core.dispatch_line(br#"{"cmd":"version","args":{"bogus":1}}"#).ok);
    }
}
