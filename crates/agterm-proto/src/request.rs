//! The request line: `{cmd, target?, args?}`, one per connection, newline-delimited JSON.

use std::fmt;

use serde::de::value::MapAccessDeserializer;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::skip_serializing_none;

use crate::command::Command;

/// Cap on a request line in bytes, newline excluded. Over it the server rejects the line and closes the
/// connection; the client checks the same cap before writing.
pub const MAX_REQUEST_LINE_BYTES: usize = 1 << 20;

/// Implements `Serialize` and `Deserialize` for wire records whose derives carry `#[serde(remote = "Self")]`, so they
/// decode from a JSON object only. A derived struct deserializer also takes a positional array
/// (`["session.close", "active", null]`), which the Swift fork's `JSONDecoder` refuses; going through
/// `deserialize_map` makes that a decode error. The derived inherent `deserialize` stays lax: decode through the
/// trait (`serde_json::from_str`), never by calling `Request::deserialize` as a path.
macro_rules! object_only_serde {
    ($($ty:ident),+ $(,)?) => {$(
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $ty::serialize(self, serializer)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct ObjectVisitor;

                impl<'de> Visitor<'de> for ObjectVisitor {
                    type Value = $ty;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str(concat!("a JSON object (", stringify!($ty), ")"))
                    }

                    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<$ty, A::Error> {
                        $ty::deserialize(MapAccessDeserializer::new(map))
                    }
                }

                deserializer.deserialize_map(ObjectVisitor)
            }
        }
    )+};
}

object_only_serde!(Request, Args, PickItem, AskButton);

/// One control request. A request that is not a JSON object, whose `cmd` is unknown, or whose fields carry a wrong
/// JSON type fails to decode as a whole; unknown keys are ignored.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct Request {
    pub cmd: Command,
    /// A session or workspace id, `active`, or a unique prefix.
    pub target: Option<String>,
    pub args: Option<Args>,
}

impl Request {
    /// A request with no target and no args.
    #[must_use]
    pub fn new(cmd: Command) -> Self {
        Self {
            cmd,
            target: None,
            args: None,
        }
    }
}

/// The bag of optional command parameters. Each command reads only the fields it needs; absent ones are omitted
/// from the JSON. Key names are the wire's (`paneID`, `workspaceName`, ...).
#[skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "camelCase")]
pub struct Args {
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub targets: Option<Vec<String>>,
    pub workspace: Option<String>,
    pub workspace_name: Option<String>,
    pub create_workspace: Option<bool>,
    pub collapsed: Option<bool>,
    pub minimized: Option<bool>,
    pub force: Option<bool>,
    pub host: Option<String>,
    pub no_select: Option<bool>,
    pub text: Option<String>,
    pub select: Option<bool>,
    pub mode: Option<String>,
    pub axis: Option<String>,
    pub path: Option<String>,
    pub color: Option<String>,
    pub text_color: Option<String>,
    pub shape: Option<String>,
    pub opacity: Option<f64>,
    pub fit: Option<String>,
    pub position: Option<String>,
    pub repeats: Option<bool>,
    pub pane: Option<String>,
    #[serde(rename = "paneID")]
    pub pane_id: Option<String>,
    pub ratio: Option<f64>,
    pub ratio_delta: Option<f64>,
    pub all: Option<bool>,
    pub lines: Option<i64>,
    pub to: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub run: Option<String>,
    pub kinds: Option<Vec<String>>,
    pub limit: Option<i64>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub command: Option<String>,
    pub wait: Option<bool>,
    pub size_percent: Option<i64>,
    pub full: Option<bool>,
    pub follow: Option<bool>,
    pub message: Option<String>,
    pub detail: Option<String>,
    pub spinner: Option<String>,
    pub hide_after: Option<f64>,
    pub markdown: Option<bool>,
    pub sticky: Option<bool>,
    pub frame: Option<bool>,
    pub items: Option<Vec<PickItem>>,
    pub prompt: Option<String>,
    pub query: Option<String>,
    pub allow_custom: Option<bool>,
    pub selection: Option<String>,
    pub buttons: Option<Vec<AskButton>>,
    pub default_button: Option<String>,
    pub style: Option<String>,
    pub align: Option<String>,
    pub destructive_button: Option<String>,
    pub window: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub sidebar_width: Option<f64>,
    pub x: Option<i64>,
    pub y: Option<i64>,
    pub display: Option<i64>,
    pub status: Option<String>,
    pub blink: Option<bool>,
    pub auto_reset: Option<bool>,
    pub sound: Option<String>,
    pub light: Option<String>,
    pub dark: Option<String>,
    pub close: Option<bool>,
    pub font_size: Option<f64>,
    pub auto_size: Option<bool>,
    pub mru: Option<bool>,
    pub html: Option<String>,
    pub current: Option<bool>,
    pub navigation: Option<bool>,
    pub url: Option<String>,
    pub javascript: Option<bool>,
    pub chromeless: Option<bool>,
    pub persistent: Option<bool>,
    pub browse: Option<bool>,
    pub value: Option<String>,
    pub page: Option<String>,
}

/// One caller-provided choice for `pick.open`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct PickItem {
    pub id: String,
    pub label: String,
    pub subtitle: Option<String>,
}

/// One caller-provided dialog button for `ask.open`.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct AskButton {
    pub id: String,
    pub label: String,
    /// An optional ASCII letter that activates the button.
    pub hotkey: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::command::{PickCmd, SessionCmd};

    #[test]
    fn bare_request_omits_target_and_args() {
        let json = serde_json::to_value(Request::new(Command::Tree)).unwrap();
        assert_eq!(json, json!({"cmd": "tree"}));
    }

    #[test]
    fn full_request_round_trips() {
        let wire = json!({
            "cmd": "session.new",
            "target": "active",
            "args": {"name": "build", "paneID": "p-1", "noSelect": true, "workspaceName": "w", "ratioDelta": 0.25,
                     "sizePercent": 40, "targets": ["a", "b"]}
        });
        let request: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(request.cmd, Command::Session(SessionCmd::New));
        assert_eq!(request.target.as_deref(), Some("active"));
        let args = request.args.clone().unwrap();
        assert_eq!(args.pane_id.as_deref(), Some("p-1"));
        assert_eq!(args.no_select, Some(true));
        assert_eq!(args.size_percent, Some(40));
        assert_eq!(serde_json::to_value(&request).unwrap(), wire);
    }

    #[test]
    fn nested_arg_items_round_trip() {
        let wire = json!({
            "cmd": "pick.open",
            "args": {"items": [{"id": "1", "label": "one"}, {"id": "2", "label": "two", "subtitle": "s"}],
                     "buttons": [{"id": "ok", "label": "OK", "hotkey": "o"}]}
        });
        let request: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(request.cmd, Command::Pick(PickCmd::Open));
        assert_eq!(serde_json::to_value(&request).unwrap(), wire);
    }

    #[test]
    fn unknown_arg_keys_are_ignored_and_null_is_absent() {
        let request: Request =
            serde_json::from_str(r#"{"cmd":"version","target":null,"args":{"bogus":1,"name":null}}"#).unwrap();
        assert_eq!(request.target, None);
        assert_eq!(request.args, Some(Args::default()));
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"cmd": "version", "args": {}})
        );
    }

    #[test]
    fn undecodable_requests_fail_as_a_whole() {
        for line in [
            r#"{"cmd":"nope"}"#,
            r#"{"target":"active"}"#,
            r#"{"cmd":"tree","target":5}"#,
            r#"{"cmd":"session.new","args":{"noSelect":"yes"}}"#,
            r#"{"cmd":"session.text","args":{"lines":"3"}}"#,
            r#"{"cmd":"pick.open","args":{"items":[{"id":"1"}]}}"#,
            r#"["tree"]"#,
        ] {
            assert!(serde_json::from_str::<Request>(line).is_err(), "{line}");
        }
    }

    #[test]
    fn positional_arrays_are_refused_at_every_level() {
        for line in [
            r#"["session.close","active",null]"#,
            r#"["session.close","active",{}]"#,
            r#"{"cmd":"session.new","args":["build"]}"#,
            r#"{"cmd":"pick.open","args":{"items":[["1","one",null]]}}"#,
            r#"{"cmd":"ask.open","args":{"buttons":[["ok","OK",null]]}}"#,
        ] {
            let error = serde_json::from_str::<Request>(line).unwrap_err();
            assert!(error.to_string().contains("expected a JSON object"), "{line}: {error}");
        }
    }

    #[test]
    fn every_arg_key_uses_the_wire_spelling() {
        let args = Args {
            pane_id: Some(String::new()),
            hide_after: Some(1.5),
            font_size: Some(12.0),
            ..Args::default()
        };
        let Value::Object(map) = serde_json::to_value(args).unwrap() else {
            panic!("args is not an object")
        };
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(keys, ["fontSize", "hideAfter", "paneID"]);
    }
}
