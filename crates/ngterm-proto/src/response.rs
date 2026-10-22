//! The single response written back per connection: `{ok, result?, error?}`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;

/// A control response. `ok` gates the meaning: on success `result` carries the payload (absent for commands with
/// nothing to report); on failure `error` carries the message, and some failures also carry a `result` (an
/// `events.read` cursor error returns the ring's page). The result type defaults to raw JSON; a client that knows the
/// command can decode a typed result instead.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response<R = Value> {
    pub ok: bool,
    pub result: Option<R>,
    pub error: Option<String>,
}

impl<R> Response<R> {
    /// `{"ok": true}` with no payload.
    #[must_use]
    pub fn success() -> Self {
        Self {
            ok: true,
            result: None,
            error: None,
        }
    }

    /// `{"ok": true, "result": ...}`.
    #[must_use]
    pub fn with_result(result: R) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// `{"ok": false, "error": ...}`.
    #[must_use]
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }

    /// `{"ok": false, "error": ..., "result": ...}`: a failure that still reports state.
    #[must_use]
    pub fn failure_with_result(error: impl Into<String>, result: R) -> Self {
        Self {
            ok: false,
            result: Some(result),
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn success_without_result_is_bare_ok() {
        let json = serde_json::to_value(Response::<Value>::success()).unwrap();
        assert_eq!(json, json!({"ok": true}));
    }

    #[test]
    fn success_with_result_round_trips() {
        let wire = json!({"ok": true, "result": {"id": "abc", "affected": 2}});
        let response: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(response, Response::with_result(json!({"id": "abc", "affected": 2})));
        assert_eq!(serde_json::to_value(&response).unwrap(), wire);
    }

    #[test]
    fn failure_round_trips() {
        let wire = json!({"ok": false, "error": "no such session: x"});
        let response: Response = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(response, Response::failure("no such session: x"));
        assert_eq!(serde_json::to_value(&response).unwrap(), wire);
    }

    #[test]
    fn failure_may_carry_a_result() {
        let wire = json!({"ok": false, "error": "cursor expired", "result": {"events": {"cursor": 7, "events": []}}});
        let response: Response = serde_json::from_value(wire.clone()).unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("cursor expired"));
        assert_eq!(response.result, Some(json!({"events": {"cursor": 7, "events": []}})));
        assert_eq!(serde_json::to_value(&response).unwrap(), wire);
        let built = Response::failure_with_result("cursor expired", json!({"events": {"cursor": 7, "events": []}}));
        assert_eq!(built, response);
    }

    #[test]
    fn null_fields_decode_as_absent() {
        let response: Response = serde_json::from_str(r#"{"ok":true,"result":null,"error":null}"#).unwrap();
        assert_eq!(response, Response::success());
    }

    #[test]
    fn ok_is_required() {
        assert!(serde_json::from_str::<Response>(r#"{"result":{}}"#).is_err());
    }
}
