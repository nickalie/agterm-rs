//! `agtermctl tree` (`agtermctlKit/Commands.swift` `Tree`): the workspace and session tree of a window, as
//! `formatTree` in `SocketClient.swift` renders it.

use agterm_proto::tree::{ControlSessionNode, ControlTree};
use agterm_proto::{Command, Request};

use crate::{BasicOptions, ClientOptions, RequestCommand};

/// `agtermctl tree [--socket <socket>] [--json] [--window <window>]`.
#[derive(Debug, clap::Args)]
#[command(about = "Print the workspace/session tree.")]
pub struct Tree {
    #[command(flatten)]
    options: ClientOptions,
}

impl RequestCommand for Tree {
    fn options(&self) -> &BasicOptions {
        &self.options.basic
    }

    fn make_request(&self) -> Request {
        Request {
            args: self.options.with_window(None),
            ..Request::new(Command::Tree)
        }
    }
}

/// The tree as an indented workspace → session listing (no trailing newline): `*` marks the current workspace and
/// the selected session, each session tagged with its split, realization, overlay and scratch state.
#[must_use]
pub fn format_tree(tree: &ControlTree) -> String {
    let mut lines = Vec::new();
    for workspace in &tree.workspaces {
        let mark = if workspace.active { "*" } else { " " };
        lines.push(format!("{mark} {}  [{}]", workspace.name, workspace.id));
        lines.extend(workspace.sessions.iter().map(format_session));
    }
    lines.join("\n")
}

fn format_session(session: &ControlSessionNode) -> String {
    let mark = if session.active { "*" } else { " " };
    // a hidden split still owns a live pane, so it is tagged apart from a session without one
    let mut tags = if session.split {
        " (split)"
    } else if session.has_split == Some(true) {
        " (split hidden)"
    } else {
        ""
    }
    .to_owned();
    if session.realized == Some(false) {
        tags.push_str(" (not realized)");
    }
    if session.overlay {
        tags.push_str(" (overlay)");
    }
    if session.scratch {
        tags.push_str(" (scratch)");
    }
    format!(
        "  {mark} {}{tags}  [{}]  {}{}",
        session.name,
        session.id,
        session.cwd,
        session_suffix(session)
    )
}

/// What follows the cwd: the split pane's own cwd, the title, live attribution and the presentation state.
fn session_suffix(session: &ControlSessionNode) -> String {
    let labelled = |label: &str, value: Option<&String>| value.map(|value| format!("  {label}: {value}"));
    let split_cwd = session.split_cwd.as_ref().filter(|cwd| **cwd != session.cwd);
    let presentation = session.presentation.as_ref().map(|presentation| {
        let presenter = if presentation.mode == "presenter" {
            ", presenter"
        } else {
            ""
        };
        let error = presentation
            .error
            .as_ref()
            .map(|error| format!(" ({error})"))
            .unwrap_or_default();
        format!("  presentation: {}{presenter}{error}", presentation.state)
    });
    let presenters = session.presenters.as_ref().map(|presenters| {
        let remote = if presenters.presenter == Some(true) {
            "  presented remotely"
        } else {
            ""
        };
        let mirrors = if presenters.mirrors > 0 {
            format!("  mirrored by: {}", presenters.mirrors)
        } else {
            String::new()
        };
        format!("{remote}{mirrors}")
    });
    [
        labelled("split cwd", split_cwd),
        labelled("title", session.title.as_ref()),
        labelled("live attribution", session.live_attribution.as_ref()),
        labelled("split live attribution", session.split_live_attribution.as_ref()),
        presentation,
        presenters,
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tree(value: serde_json::Value) -> ControlTree {
        serde_json::from_value(value).unwrap()
    }

    fn session(id: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut node = json!({"id": id, "name": id, "cwd": "/w", "active": false, "split": false, "overlay": false,
                              "scratch": false, "flagged": false});
        if let (Some(node), serde_json::Value::Object(extra)) = (node.as_object_mut(), extra) {
            node.extend(extra);
        }
        node
    }

    #[test]
    fn marks_the_current_workspace_and_selected_session() {
        let rendered = format_tree(&tree(json!({"workspaces": [
            {"id": "W1", "name": "one", "active": true, "sessions": [session("a", json!({"active": true}))]},
            {"id": "W2", "name": "two", "active": false, "sessions": [session("b", json!({}))]},
        ]})));
        assert_eq!(rendered, "* one  [W1]\n  * a  [a]  /w\n  two  [W2]\n    b  [b]  /w");
    }

    #[test]
    fn tags_each_session_state_in_order() {
        let rendered = format_tree(&tree(
            json!({"workspaces": [{"id": "W", "name": "w", "active": true, "sessions": [
                session("shown", json!({"split": true, "hasSplit": true})),
                session("hidden", json!({"hasSplit": true, "realized": false})),
                session("covered", json!({"overlay": true, "scratch": true})),
            ]}]}),
        ));
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[1], "    shown (split)  [shown]  /w");
        assert_eq!(lines[2], "    hidden (split hidden) (not realized)  [hidden]  /w");
        assert_eq!(lines[3], "    covered (overlay) (scratch)  [covered]  /w");
    }

    #[test]
    fn the_suffix_carries_split_cwd_title_attribution_and_presentation() {
        let node = session(
            "s",
            json!({"splitCwd": "/x", "title": "t", "liveAttribution": "app", "splitLiveAttribution": "unknown",
                   "presentation": {"state": "failed", "mode": "presenter", "error": "boom"},
                   "presenters": {"mirrors": 2, "presenter": true}}),
        );
        let rendered = format_tree(&tree(json!({"workspaces": [{"id": "W", "name": "w", "active": false,
                                                                 "sessions": [node]}]})));
        assert_eq!(
            rendered.lines().nth(1),
            Some(
                "    s  [s]  /w  split cwd: /x  title: t  live attribution: app  split live attribution: unknown  \
                 presentation: failed, presenter (boom)  presented remotely  mirrored by: 2"
            )
        );
    }

    #[test]
    fn a_split_cwd_equal_to_the_cwd_and_zero_mirrors_print_nothing() {
        let node = session("s", json!({"splitCwd": "/w", "presenters": {"mirrors": 0}}));
        let rendered = format_tree(&tree(json!({"workspaces": [{"id": "W", "name": "w", "active": false,
                                                                 "sessions": [node]}]})));
        assert_eq!(rendered.lines().nth(1), Some("    s  [s]  /w"));
    }

    #[test]
    fn an_empty_tree_prints_nothing() {
        assert_eq!(format_tree(&ControlTree::default()), "");
    }
}
