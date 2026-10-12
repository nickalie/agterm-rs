//! Control errors. Each variant's `Display` is the wire `error` string, verbatim from the Swift fork
//! (`ControlResolve.swift`, `ControlServer.swift`, `ControlActionsDefaults.swift`) unless noted.

use agterm_proto::{Command, Response};

/// What a target names, as the error messages spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noun {
    Session,
    Workspace,
    Window,
}

impl Noun {
    /// The lowercase word used in messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Window => "window",
        }
    }
}

impl std::fmt::Display for Noun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed control request. Converts into an `ok:false` [`Response`] carrying the `Display` text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// The request line is not JSON, not an object, names no known command, or has a field of the wrong type
    /// (`protocol.malformed-request`). The fork's text; upstream's differs and the suite checks neither.
    #[error("could not decode request")]
    MalformedRequest,

    /// A target matched nothing.
    #[error("no such {noun}: {target}")]
    NotFound { noun: Noun, target: String },

    /// A target prefix matched several ids; `hits` are the full ids, listed by their first 8 characters.
    #[error("ambiguous {noun} prefix '{target}' → {}", short_ids(hits))]
    Ambiguous {
        noun: Noun,
        target: String,
        hits: Vec<String>,
    },

    /// A command this build does not implement (yet).
    #[error("{0} is not supported by this host")]
    NotSupported(Command),

    /// The handler panicked; the panic is logged. Rust build only: Swift traps instead.
    #[error("internal error handling {0}")]
    HandlerPanicked(Command),

    /// The main loop shut down before it answered. Rust build only.
    #[error("the app is shutting down")]
    ShuttingDown,

    /// A result could not be serialized. Rust build only: the fork writes nothing at all in that case.
    #[error("could not encode response")]
    Encode,
}

fn short_ids(hits: &[String]) -> String {
    hits.iter()
        .map(|id| id.chars().take(8).collect::<String>())
        .collect::<Vec<_>>()
        .join(", ")
}

impl From<ControlError> for Response {
    fn from(error: ControlError) -> Self {
        Self::failure(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use agterm_proto::command::SessionCmd;
    use serde_json::json;

    use super::*;

    #[test]
    fn messages_match_the_fork() {
        let cases = [
            (ControlError::MalformedRequest, "could not decode request"),
            (
                ControlError::NotFound {
                    noun: Noun::Session,
                    target: "zzz".to_owned(),
                },
                "no such session: zzz",
            ),
            (
                ControlError::Ambiguous {
                    noun: Noun::Workspace,
                    target: "a".to_owned(),
                    hits: vec![
                        "A1B2C3D4-0000-0000-0000-000000000000".to_owned(),
                        "A9B8C7D6-1111-1111-1111-111111111111".to_owned(),
                    ],
                },
                "ambiguous workspace prefix 'a' → A1B2C3D4, A9B8C7D6",
            ),
            (
                ControlError::NotSupported(Command::Session(SessionCmd::Swap)),
                "session.swap is not supported by this host",
            ),
            (
                ControlError::HandlerPanicked(Command::Tree),
                "internal error handling tree",
            ),
        ];
        for (error, text) in cases {
            assert_eq!(error.to_string(), text);
        }
    }

    #[test]
    fn converts_into_a_failure_response() {
        let response = Response::from(ControlError::NotFound {
            noun: Noun::Window,
            target: "w".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({"ok": false, "error": "no such window: w"})
        );
    }
}
