//! Ported from upstream's `AgentHooksInstallTests.swift`.

use serde_json::Value;

use super::*;

const SCRIPT_DIR: &str = "/home/me/.config/agterm/agent-status";

fn events(json: &str) -> Map<String, Value> {
    let root: Value = serde_json::from_str(json).unwrap();
    root["hooks"].as_object().cloned().unwrap_or_default()
}

fn commands(json: &str, event: &str) -> Vec<String> {
    events(json)[event]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["hooks"][0]["command"].as_str().map(str::to_owned))
        .collect()
}

fn adapter() -> String {
    format!("{SCRIPT_DIR}/{CLAUDE_WRAPPER_NAME}")
}

fn wrapper() -> String {
    format!("{SCRIPT_DIR}/{WRAPPER_NAME}")
}

fn merge(existing: Option<&str>) -> (String, bool) {
    merge_claude_settings(existing, SCRIPT_DIR).unwrap()
}

#[test]
fn a_fresh_merge_adds_all_five_hooks_through_the_adapter() {
    let (json, changed) = merge(None);
    assert!(changed);
    let hooks = events(&json);
    assert_eq!(hooks.len(), 5);
    let a = adapter();
    assert_eq!(commands(&json, "UserPromptSubmit"), [format!("'{a}' active --blink")]);
    assert_eq!(commands(&json, "PostToolUse"), [format!("'{a}' active --blink")]);
    assert_eq!(commands(&json, "Stop"), [format!("'{a}' completed --auto-reset")]);
    assert_eq!(commands(&json, "Notification"), [format!("'{a}' blocked")]);
    assert_eq!(commands(&json, "PermissionRequest"), [format!("'{a}' blocked")]);
    assert_eq!(hooks["Notification"][0]["matcher"], "permission_prompt");
    assert!(hooks["PermissionRequest"][0].get("matcher").is_none());
    assert!(json.ends_with("}\n"));
}

#[test]
fn the_merged_settings_sort_their_keys() {
    let (json, _) = merge(Some(r#"{"model": "opus", "alpha": {"z": 1, "a": 2}}"#));
    let alpha = json.find("\"alpha\"").unwrap();
    let hooks = json.find("\"hooks\"").unwrap();
    let model = json.find("\"model\"").unwrap();
    assert!(alpha < hooks && hooks < model, "{json}");
    assert!(json.find("\"a\": 2").unwrap() < json.find("\"z\": 1").unwrap());
    let entry = json.find("\"command\"").unwrap();
    assert!(entry < json.find("\"type\"").unwrap());
}

/// Managed-marker idempotence: a second merge recognizes the hooks it wrote and changes nothing.
#[test]
fn a_second_merge_is_a_no_op() {
    let (first, _) = merge(None);
    let (second, changed) = merge(Some(&first));
    assert!(!changed);
    assert_eq!(second, first);
}

#[test]
fn unrelated_hooks_and_keys_survive() {
    let existing = r#"{
      "model": "opus",
      "hooks": {
        "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "/usr/bin/other-hook.sh"}]}],
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/bin/guard.sh"}]}],
        "Odd": "any shape round-trips"
      }
    }"#;
    let (json, changed) = merge(Some(existing));
    assert!(changed);
    let root: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(root["model"], "opus");
    assert_eq!(commands(&json, "PreToolUse"), ["/usr/bin/guard.sh"]);
    let prompts = commands(&json, "UserPromptSubmit");
    assert_eq!(prompts.len(), 2);
    assert!(prompts.contains(&"/usr/bin/other-hook.sh".to_owned()));
    assert_eq!(events(&json)["Odd"], "any shape round-trips");
    let (again, changed) = merge(Some(&json));
    assert!(!changed);
    assert_eq!(again, json);
}

fn legacy_settings(extra_user_hook: Option<&str>) -> String {
    let w = wrapper();
    let user = extra_user_hook
        .map(|hook| format!(r#", {{"hooks": [{{"type": "command", "command": "{hook}"}}]}}"#))
        .unwrap_or_default();
    format!(
        r#"{{"hooks": {{
          "UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "'{w}' active --blink"}}]}}{user}],
          "PostToolUse": [{{"hooks": [{{"type": "command", "command": "'{w}' active --blink"}}]}}],
          "Stop": [{{"hooks": [{{"type": "command", "command": "'{w}' completed --auto-reset"}}]}}],
          "Notification": [{{"matcher": "permission_prompt", "hooks": [{{"type": "command", "command": "'{w}' blocked"}}]}}]
        }}}}"#
    )
}

#[test]
fn an_earlier_install_migrates_onto_the_adapter_in_place() {
    let (json, changed) = merge(Some(&legacy_settings(None)));
    assert!(changed);
    let a = adapter();
    assert_eq!(commands(&json, "UserPromptSubmit"), [format!("'{a}' active --blink")]);
    assert_eq!(commands(&json, "Stop"), [format!("'{a}' completed --auto-reset")]);
    assert_eq!(commands(&json, "Notification"), [format!("'{a}' blocked")]);
    assert_eq!(commands(&json, "PermissionRequest"), [format!("'{a}' blocked")]);
    assert_eq!(events(&json)["Notification"][0]["matcher"], "permission_prompt");
    assert!(!json.contains(&format!("{}'", wrapper())));
    let (again, changed) = merge(Some(&json));
    assert!(!changed, "the migration is idempotent");
    assert_eq!(again, json);
}

/// Upstream's `mergeAddsPermissionRequestToFourEventInstall`: an install from before `PermissionRequest` existed,
/// already on the adapter, gains only that hook.
#[test]
fn a_four_event_adapter_install_gains_permission_request() {
    let a = adapter();
    let existing = format!(
        r#"{{"hooks": {{
          "UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "'{a}' active --blink"}}]}}],
          "PostToolUse": [{{"hooks": [{{"type": "command", "command": "'{a}' active --blink"}}]}}],
          "Stop": [{{"hooks": [{{"type": "command", "command": "'{a}' completed --auto-reset"}}]}}],
          "Notification": [{{"matcher": "permission_prompt", "hooks": [{{"type": "command", "command": "'{a}' blocked"}}]}}]
        }}}}"#
    );
    let (first, changed) = merge(Some(&existing));
    assert!(changed);
    let hooks = events(&first);
    assert_eq!(hooks.len(), 5);
    for event in [
        "UserPromptSubmit",
        "PostToolUse",
        "Stop",
        "Notification",
        "PermissionRequest",
    ] {
        assert_eq!(hooks[event].as_array().map(Vec::len), Some(1), "{event}");
    }
    assert_eq!(commands(&first, "PermissionRequest"), [format!("'{a}' blocked")]);
    assert!(hooks["PermissionRequest"][0].get("matcher").is_none());
    assert_eq!(hooks["Notification"][0]["matcher"], "permission_prompt");
    let (second, changed) = merge(Some(&first));
    assert!(!changed);
    assert_eq!(second, first);
}

#[test]
fn the_pre_blink_prompt_entry_migrates_to_the_current_state() {
    let existing = format!(
        r#"{{"hooks": {{"UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "'{}' active"}}]}}]}}}}"#,
        wrapper()
    );
    let (json, _) = merge(Some(&existing));
    assert_eq!(
        commands(&json, "UserPromptSubmit"),
        [format!("'{}' active --blink", adapter())]
    );
}

#[test]
fn a_customized_or_user_entry_naming_the_wrapper_is_left_alone() {
    let customized = format!("'{}' active --blink --pane right", wrapper());
    let existing = format!(
        r#"{{"hooks": {{
          "UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "{customized}"}}]}}],
          "Stop": [{{"hooks": [{{"type": "command", "command": "'{}' completed --auto-reset"}}]}}]
        }}}}"#,
        wrapper()
    );
    let (json, _) = merge(Some(&existing));
    assert_eq!(
        commands(&json, "UserPromptSubmit"),
        [customized],
        "no stock entry beside it"
    );
    assert_eq!(
        commands(&json, "Stop"),
        [format!("'{}' completed --auto-reset", adapter())]
    );

    let user_hook = format!("my-notifier.sh && '{}' active --blink", wrapper());
    let (json, _) = merge(Some(&legacy_settings(Some(&user_hook))));
    let prompts = commands(&json, "UserPromptSubmit");
    assert!(prompts.contains(&user_hook));
    assert!(prompts.contains(&format!("'{}' active --blink", adapter())));
    assert_eq!(prompts.len(), 2);
}

#[test]
fn malformed_settings_are_refused_not_overwritten() {
    for malformed in [
        "{ this is not json",
        "[1, 2, 3]",
        r#"{"hooks": "nope"}"#,
        r#"{"hooks": [1, 2]}"#,
        r#"{"hooks": {"Stop": "nope"}}"#,
        r#"{"hooks": {"Stop": [1, 2]}}"#,
        r#"{"hooks": {"Stop": {"command": "x"}}}"#,
    ] {
        assert_eq!(
            merge_claude_settings(Some(malformed), SCRIPT_DIR),
            Err(MalformedSettings),
            "{malformed}"
        );
    }
}

#[test]
fn a_blank_settings_file_starts_fresh() {
    for blank in ["", "   \n\t\n"] {
        let (json, changed) = merge(Some(blank));
        assert!(changed);
        assert_eq!(events(&json).len(), 5);
    }
}

#[test]
fn the_codex_block_maps_every_event_to_the_adapter() {
    let block = codex_hooks_block(SCRIPT_DIR);
    let hook = format!("{SCRIPT_DIR}/agterm-codex-status.sh");
    for (event, action) in CODEX_HOOKS {
        assert!(block.contains(&format!(
            "[[hooks.{event}]]\n[[hooks.{event}.hooks]]\ntype = \"command\"\n"
        )));
        assert!(block.contains(&format!("command = \"'{hook}' {action}\"")));
    }
    assert!(!block.ends_with('\n'));
}

#[test]
fn the_codex_block_quotes_spaces_and_escapes_apostrophes() {
    let block = codex_hooks_block("/home/my name/agent-status");
    assert!(block.contains("command = \"'/home/my name/agent-status/agterm-codex-status.sh' session-start\""));
    let block = codex_hooks_block("/home/O'Brien/agent-status");
    assert!(block.contains("'/home/O'\\\\''Brien/agent-status/agterm-codex-status.sh' session-start"));
    let parsed: toml::Table = format!("{block}\n").parse().unwrap();
    let command = parsed["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(command, "'/home/O'\\''Brien/agent-status/agterm-codex-status.sh' stop");
}

fn merged(outcome: CodexMerge) -> String {
    match outcome {
        CodexMerge::Merged(contents) => contents,
        other => panic!("expected Merged, got {other:?}"),
    }
}

#[test]
fn codex_merge_appends_to_an_empty_config_and_is_idempotent() {
    let contents = merged(merge_codex_config("", SCRIPT_DIR));
    assert!(contents.starts_with(RC_MARKER_BEGIN) && contents.ends_with(&format!("{RC_MARKER_END}\n")));
    assert!(contents.parse::<toml::Table>().is_ok());
    let first = merged(merge_codex_config("model = \"gpt-5\"\n", SCRIPT_DIR));
    assert!(first.starts_with("model = \"gpt-5\"\n\n# >>>"));
    assert_eq!(merge_codex_config(&first, SCRIPT_DIR), CodexMerge::Unchanged);
    assert_eq!(first.matches(RC_MARKER_BEGIN).count(), 1);
}

#[test]
fn codex_merge_refreshes_a_managed_block_and_keeps_the_trust_state() {
    let legacy = wrapper();
    let existing = format!(
        "model = \"gpt-5\"\n\n{RC_MARKER_BEGIN}\n[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\n\
         command = \"'{legacy}' idle\"\n\n[[hooks.PermissionRequest]]\n[[hooks.PermissionRequest.hooks]]\ntype = \"command\"\n\
         command = \"'{legacy}' blocked\"\n\n[hooks.state]\n\n[hooks.state.\"/home/me/.codex/config.toml:session_start:0:0\"]\n\
         trusted_hash = \"sha256:stale-but-preserved\"\n{RC_MARKER_END}"
    );
    let contents = merged(merge_codex_config(&existing, SCRIPT_DIR));
    let hook = format!("{SCRIPT_DIR}/agterm-codex-status.sh");
    for (_, action) in CODEX_HOOKS {
        assert!(contents.contains(&format!("'{hook}' {action}")), "{action}");
    }
    assert!(!contents.contains(&format!("'{legacy}' blocked")));
    assert!(contents.contains(
        "command = \"'/home/me/.config/agterm/agent-status/agterm-codex-status.sh' stop\"\n\n[hooks.state]\n"
    ));
    assert!(contents.contains("trusted_hash = \"sha256:stale-but-preserved\"\n# <<<"));
    assert_eq!(contents.matches(RC_MARKER_BEGIN).count(), 1);
    assert_eq!(merge_codex_config(&contents, SCRIPT_DIR), CodexMerge::Unchanged);
}

#[test]
fn codex_merge_leaves_foreign_markers_own_hooks_and_bad_toml_alone() {
    let foreign = format!("{RC_MARKER_BEGIN}\n# user content\nmodel = \"gpt-5\"\n{RC_MARKER_END}");
    assert_eq!(merge_codex_config(&foreign, SCRIPT_DIR), CodexMerge::Unchanged);
    let own = "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo done\"\n";
    assert_eq!(merge_codex_config(own, SCRIPT_DIR), CodexMerge::HooksExist);
    assert_eq!(
        merge_codex_config("this = is = not = toml\n", SCRIPT_DIR),
        CodexMerge::Unparseable
    );
}

#[test]
fn codex_merge_removes_only_the_retired_notify_value() {
    let legacy = "notify = [\"/home/me/.config/agterm/agent-status/codex-notify.sh\"]\n";
    let contents = merged(merge_codex_config(legacy, SCRIPT_DIR));
    assert!(!contents.contains("codex-notify.sh"));
    assert!(contents.contains("[[hooks.Stop]]"));

    let own = "# my codex config\nmodel = \"gpt-5\"\nnotify = [\"/home/me/my-own-notify.sh\"]";
    let contents = merged(merge_codex_config(own, SCRIPT_DIR));
    assert!(contents.starts_with(own));
    assert!(contents.contains(&format!("my-own-notify.sh\"]\n\n{RC_MARKER_BEGIN}")));

    let multiline =
        "notify = [\"/home/me/.config/agterm/agent-status/codex-notify.sh\",\n  \"--flag\"]\nmodel = \"x\"\n";
    let contents = merged(merge_codex_config(multiline, SCRIPT_DIR));
    assert!(
        contents.starts_with(multiline),
        "a multi-line notify is kept whole, not half-cut"
    );
    assert!(
        contents.parse::<toml::Table>().is_ok(),
        "the merged config stays valid TOML"
    );

    let comment = "notify = [\"/home/me/custom.sh\"] # replaces codex-notify.sh\n";
    let contents = merged(merge_codex_config(comment, SCRIPT_DIR));
    assert!(contents.contains("notify = [\"/home/me/custom.sh\"]"));
}

#[test]
fn the_rc_block_is_appended_once() {
    let (contents, changed) = append_shell_rc("export FOO=1\n", SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    assert!(changed);
    assert_eq!(
        contents,
        format!("export FOO=1\n\n{RC_MARKER_BEGIN}\nsource '{SCRIPT_DIR}/shell/integration.sh'\n{RC_MARKER_END}\n")
    );
    let (again, changed) = append_shell_rc(&contents, SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    assert!(!changed);
    assert_eq!(again, contents);
    let (fresh, _) = append_shell_rc("", SCRIPT_DIR, FISH_INTEGRATION_RELATIVE_PATH);
    assert!(fresh.starts_with(RC_MARKER_BEGIN));
    assert!(fresh.contains("source '/home/me/.config/agterm/agent-status/shell/integration.fish'"));
    let (unterminated, _) = append_shell_rc("export FOO=1", SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    assert!(unterminated.starts_with("export FOO=1\n\n# >>>"));
}

#[test]
fn pi_paths_and_ownership() {
    assert_eq!(
        pi_extension_path("/home/me"),
        "/home/me/.pi/agent/extensions/agterm-status.ts"
    );
    assert!(may_overwrite_pi_extension(false, None));
    assert!(may_overwrite_pi_extension(
        true,
        Some("// agterm-pi-status-extension\nexport default () => {}\n")
    ));
    assert!(!may_overwrite_pi_extension(true, Some("export default () => {}\n")));
    assert!(!may_overwrite_pi_extension(true, None));
}

#[test]
fn backup_paths_append_bak() {
    assert_eq!(
        backup_path("/home/me/.claude/settings.json"),
        "/home/me/.claude/settings.json.bak"
    );
    assert_eq!(backup_path("/home/me/.zshrc"), "/home/me/.zshrc.bak");
}

const WRAPPER_STUB: &str = "#!/usr/bin/env bash\nset -u\n\"${AGTERMCTL:-agtermctl}\" session status \"$1\"";

#[test]
fn baking_inserts_the_block_after_the_shebang_with_a_path_fallback() {
    let baked = bake_agtermctl_path(WRAPPER_STUB, "/opt/agterm/bin/agtermctl");
    let lines: Vec<&str> = baked.split('\n').collect();
    assert_eq!(lines[0], "#!/usr/bin/env bash");
    assert_eq!(lines[1], AGTERMCTL_MARKER);
    assert!(lines.contains(&"  AGTERMCTL='/opt/agterm/bin/agtermctl'"));
    assert!(baked.contains(r#"  [ -x "$AGTERMCTL" ] || AGTERMCTL="$(command -v agtermctl || true)""#));
    assert!(baked.ends_with("set -u\n\"${AGTERMCTL:-agtermctl}\" session status \"$1\""));
}

#[test]
fn rebaking_replaces_the_block_instead_of_stacking_it() {
    let first = bake_agtermctl_path(WRAPPER_STUB, "/tmp/old/agtermctl");
    let moved = bake_agtermctl_path(&first, "/opt/agterm/bin/agtermctl");
    assert_eq!(moved.matches(AGTERMCTL_MARKER).count(), 1);
    assert!(!moved.contains("/tmp/old"));
    assert_eq!(moved, bake_agtermctl_path(WRAPPER_STUB, "/opt/agterm/bin/agtermctl"));
}
