//! Ported from upstream's `CustomCommandTests` (the Codable round trip has no counterpart here).

use std::collections::{BTreeSet, HashMap};

use super::*;

fn sample_context() -> CommandContext {
    CommandContext {
        session_id: "sess-1".to_owned(),
        session_name: "shell".to_owned(),
        session_pwd: "/tmp/work".to_owned(),
        session_host: "user@box".to_owned(),
        workspace_id: "ws-1".to_owned(),
        workspace_name: "main".to_owned(),
        window_id: "win-1".to_owned(),
        window_name: "work".to_owned(),
        pane: CommandPane::Right,
        pane_id: "pane-tok".to_owned(),
        selection: "hello".to_owned(),
        socket: "/tmp/agt.sock".to_owned(),
    }
}

fn env(context: &CommandContext) -> HashMap<String, String> {
    context.environment().into_iter().collect()
}

#[test]
fn session_host_follows_the_pwd_token_and_defaults_empty() {
    let context = sample_context();
    assert_eq!(context.expand("ssh {AGT_SESSION_HOST}"), "ssh user@box");
    assert_eq!(env(&context)["AGT_SESSION_HOST"], "user@box");
    assert_eq!(CommandContext::default().expand("[{AGT_SESSION_HOST}]"), "[]");
    assert_eq!(env(&CommandContext::default())["AGT_SESSION_HOST"], "");
    let names = CommandContext::token_names();
    let pwd = names.iter().position(|name| *name == "AGT_SESSION_PWD");
    assert_eq!(pwd.map(|index| names[index + 1]), Some("AGT_SESSION_HOST"));
    assert!(CommandContext::references_session_scoped_context(
        r#"ssh "$AGT_SESSION_HOST" uptime"#
    ));
}

#[test]
fn expand_substitutes_known_tokens() {
    let context = sample_context();
    assert_eq!(context.expand("open {AGT_SESSION_PWD}"), "open /tmp/work");
    assert_eq!(context.expand("{AGT_WINDOW_NAME}/{AGT_WORKSPACE_NAME}"), "work/main");
}

#[test]
fn expand_repeats_a_token() {
    assert_eq!(
        sample_context().expand("{AGT_SESSION_ID} {AGT_SESSION_ID}"),
        "sess-1 sess-1"
    );
}

#[test]
fn an_empty_token_value_expands_to_an_empty_string() {
    let context = CommandContext {
        session_pwd: "/tmp".to_owned(),
        ..CommandContext::default()
    };
    assert_eq!(context.expand("[{AGT_SELECTION}]"), "[]");
    assert_eq!(context.expand("cd {AGT_SESSION_PWD}"), "cd /tmp");
}

#[test]
fn expand_leaves_unknown_braces_untouched() {
    assert_eq!(sample_context().expand("echo {FOO} ${BAR}"), "echo {FOO} ${BAR}");
    assert_eq!(sample_context().expand("{{AGT_PANE}}"), "{{AGT_PANE}}");
}

#[test]
fn expand_without_tokens_is_the_identity() {
    assert_eq!(sample_context().expand("git status"), "git status");
    assert_eq!(sample_context().expand(""), "");
}

#[test]
fn expand_does_not_re_substitute_a_token_inside_a_value() {
    let context = CommandContext {
        selection: "{AGT_SOCKET}".to_owned(),
        socket: "/tmp/agt.sock".to_owned(),
        ..CommandContext::default()
    };
    assert_eq!(context.expand("echo {AGT_SELECTION}"), "echo {AGT_SOCKET}");
    assert_eq!(
        context.expand("{AGT_SELECTION} {AGT_SOCKET}"),
        "{AGT_SOCKET} /tmp/agt.sock"
    );
}

#[test]
fn expand_leaves_an_unclosed_brace_untouched() {
    assert_eq!(sample_context().expand("echo {AGT_SESSION_ID"), "echo {AGT_SESSION_ID");
    assert_eq!(sample_context().expand("a {AGT_PANE} b {c"), "a right b {c");
}

#[test]
fn the_environment_has_every_token_in_order() {
    let pairs = sample_context().environment();
    let expected = [
        ("AGT_SESSION_ID", "sess-1"),
        ("AGT_SESSION_NAME", "shell"),
        ("AGT_SESSION_PWD", "/tmp/work"),
        ("AGT_SESSION_HOST", "user@box"),
        ("AGT_WORKSPACE_ID", "ws-1"),
        ("AGT_WORKSPACE_NAME", "main"),
        ("AGT_WINDOW_ID", "win-1"),
        ("AGT_WINDOW_NAME", "work"),
        ("AGT_PANE", "right"),
        ("AGT_PANE_ID", "pane-tok"),
        ("AGT_SELECTION", "hello"),
        ("AGT_SOCKET", "/tmp/agt.sock"),
    ];
    let pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    assert_eq!(pairs, expected);
}

#[test]
fn the_pane_id_defaults_empty_and_expands_beside_the_pane() {
    assert_eq!(CommandContext::default().pane_id, "");
    assert_eq!(env(&CommandContext::default())["AGT_PANE_ID"], "");
    assert_eq!(CommandContext::default().expand("[{AGT_PANE_ID}]"), "[]");
    let context = CommandContext {
        pane: CommandPane::Right,
        pane_id: "pane-tok".to_owned(),
        ..CommandContext::default()
    };
    assert_eq!(
        context.expand("--pane {AGT_PANE} --pane-id {AGT_PANE_ID}"),
        "--pane right --pane-id pane-tok"
    );
    assert!(!CommandContext::references_session_scoped_context(
        r#"ngtermctl session text --pane-id "$AGT_PANE_ID""#
    ));
}

#[test]
fn the_pane_defaults_to_left_and_scratch_expands_to_scratch() {
    assert_eq!(CommandContext::default().pane, CommandPane::Left);
    assert_eq!(env(&CommandContext::default())["AGT_PANE"], "left");
    assert_eq!(CommandContext::default().expand("{AGT_PANE}"), "left");
    let scratch = CommandContext {
        pane: CommandPane::Scratch,
        ..CommandContext::default()
    };
    assert_eq!(env(&scratch)["AGT_PANE"], "scratch");
    assert_eq!(scratch.expand("{AGT_PANE}"), "scratch");
    assert_eq!(CommandPane::Right.wire_name(), "right");
}

#[test]
fn session_scoped_tokens_are_detected_but_launchers_are_not() {
    for body in [
        "echo {AGT_SESSION_PWD}",
        r#"rm -rf "$AGT_SESSION_PWD"/*"#,
        "echo ${AGT_WORKSPACE_NAME}",
        "printf %s {AGT_SELECTION}",
    ] {
        assert!(CommandContext::references_session_scoped_context(body), "{body}");
    }
    for body in [
        r#"ngtermctl session new --command "ssh work""#,
        "ngtermctl --socket {AGT_SOCKET} session new",
        "open -a Safari {AGT_WINDOW_ID} {AGT_PANE}",
    ] {
        assert!(!CommandContext::references_session_scoped_context(body), "{body}");
    }
    assert_eq!(
        SESSION_SCOPED_TOKEN_BASES,
        ["AGT_SESSION", "AGT_WORKSPACE", "AGT_SELECTION"]
    );
}

#[test]
fn token_names_match_the_expansion_and_environment_sets() {
    let context = sample_context();
    let names: BTreeSet<&str> = CommandContext::token_names().into_iter().collect();
    let env_keys: BTreeSet<String> = env(&context).into_keys().collect();
    assert_eq!(names.len(), 12);
    assert_eq!(names, env_keys.iter().map(String::as_str).collect());
    for name in names {
        assert!(!context.expand(&format!("{{{name}}}")).contains('{'), "{name}");
    }
}

#[test]
fn a_new_command_carries_upstreams_option_defaults() {
    let command = CustomCommand::new("Zed", "open -a Zed {AGT_SESSION_PWD}", "cmd+shift+e");
    assert!(!command.error_hud);
    assert_eq!(command.error_position, crate::hud::HudPosition::Center);
    assert_eq!(command.error_pane, None);
    assert!(!command.repeats);
    assert_ne!(
        command.id,
        CustomCommand::new("Zed", "", "").id,
        "every command gets a fresh id"
    );
}
