//! Ported from upstream's `AgentHooksInstallTests.swift`.

use serde_json::Value;

use super::*;

const SCRIPT_DIR: &str = "/home/me/.config/ngterm/agent-status";

/// The directory an earlier install (the fork's, or this build's before it had its own) used.
const LEGACY_DIR: &str = "/home/me/.config/agterm/agent-status";

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

/// The restore adapter's command for `arguments` (`claude start`, `claude end`).
fn restore(arguments: &str) -> String {
    format!("'{SCRIPT_DIR}/{RESTORE_WRAPPER_NAME}' {arguments}")
}

/// The commands of `event` without the restore adapter's: the status hooks and the user's own.
fn status_commands(json: &str, event: &str) -> Vec<String> {
    let mut found = commands(json, event);
    found.retain(|command| !command.contains(RESTORE_WRAPPER_NAME));
    found
}

fn merge(existing: Option<&str>) -> (String, bool) {
    merge_claude_settings(existing, SCRIPT_DIR, &[LEGACY_DIR]).unwrap()
}

#[test]
fn a_fresh_merge_adds_all_six_hooks_through_the_adapter() {
    let (json, changed) = merge(None);
    assert!(changed);
    let hooks = events(&json);
    assert_eq!(
        hooks.len(),
        7,
        "the six status events and the restore adapter's SessionStart"
    );
    let a = adapter();
    assert_eq!(commands(&json, "UserPromptSubmit"), [format!("'{a}' active --blink")]);
    assert_eq!(commands(&json, "PostToolUse"), [format!("'{a}' active --blink")]);
    assert_eq!(commands(&json, "Stop"), [format!("'{a}' completed --auto-reset")]);
    assert_eq!(commands(&json, "Notification"), [format!("'{a}' blocked")]);
    assert_eq!(commands(&json, "PermissionRequest"), [format!("'{a}' blocked")]);
    assert_eq!(
        status_commands(&json, "SessionEnd"),
        [format!("'{a}' completed --auto-reset")]
    );
    assert_eq!(hooks["Notification"][0]["matcher"], "permission_prompt");
    assert!(hooks["PermissionRequest"][0].get("matcher").is_none());
    assert!(hooks["SessionEnd"][0].get("matcher").is_none());
    assert!(json.ends_with("}\n"));
}

#[test]
fn a_fresh_merge_adds_the_restore_hooks_after_the_status_ones() {
    let (json, _) = merge(None);
    let hooks = events(&json);
    assert_eq!(
        commands(&json, "SessionEnd"),
        [format!("'{}' completed --auto-reset", adapter()), restore("claude end")]
    );
    assert_eq!(commands(&json, "SessionStart"), [restore("claude start")]);
    assert!(hooks["SessionStart"][0].get("matcher").is_none(), "every start source");
    assert!(
        hooks["SessionEnd"][1].get("matcher").is_none(),
        "the adapter reads the reason itself"
    );
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
/// already on the adapter, gains only that hook (and this build's `SessionEnd`).
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
    assert_eq!(hooks.len(), 7);
    for event in [
        "UserPromptSubmit",
        "PostToolUse",
        "Stop",
        "Notification",
        "PermissionRequest",
        "SessionEnd",
        "SessionStart",
    ] {
        let expected = if event == "SessionEnd" { 2 } else { 1 };
        assert_eq!(hooks[event].as_array().map(Vec::len), Some(expected), "{event}");
    }
    assert_eq!(commands(&first, "PermissionRequest"), [format!("'{a}' blocked")]);
    assert!(hooks["PermissionRequest"][0].get("matcher").is_none());
    assert_eq!(hooks["Notification"][0]["matcher"], "permission_prompt");
    let (second, changed) = merge(Some(&first));
    assert!(!changed);
    assert_eq!(second, first);
}

/// An install of upstream's five events gains only `SessionEnd`, beside the user's own entry for that event.
#[test]
fn a_five_event_adapter_install_gains_session_end() {
    let mut root: Value = serde_json::from_str(&adapter_settings(SCRIPT_DIR)).unwrap();
    root["hooks"]["SessionEnd"] = serde_json::json!([{"hooks": [{"type": "command", "command": "echo bye"}]}]);
    let (first, changed) = merge(Some(&root.to_string()));
    assert!(changed);
    let a = adapter();
    assert_eq!(
        commands(&first, "SessionEnd"),
        [
            "echo bye".to_owned(),
            format!("'{a}' completed --auto-reset"),
            restore("claude end")
        ]
    );
    for (event, _, state) in CLAUDE_HOOKS {
        if event != "SessionEnd" {
            assert_eq!(commands(&first, event), [format!("'{a}' {state}")], "{event}");
        }
    }
    let (second, changed) = merge(Some(&first));
    assert!(!changed);
    assert_eq!(second, first);
}

/// No earlier installer wrote `SessionEnd`, so an entry there naming a wrapper is the user's even when it reads like a
/// stock one: the legacy clean-up repoints it rather than dropping it, and the adapter migration leaves it on the
/// generic wrapper (where, calling the package, it counts as installed).
#[test]
fn a_users_session_end_entry_naming_a_wrapper_is_never_taken_for_a_stock_one() {
    let a = adapter();
    let mut root: Value = serde_json::from_str(&adapter_settings(SCRIPT_DIR)).unwrap();
    let legacy = format!("'{LEGACY_DIR}/{WRAPPER_NAME}' completed --auto-reset");
    root["hooks"]["SessionEnd"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"hooks": [{"type": "command", "command": legacy}]}));
    let (json, changed) = merge(Some(&root.to_string()));
    assert!(changed);
    assert_eq!(
        status_commands(&json, "SessionEnd"),
        [
            format!("'{a}' completed --auto-reset"),
            format!("'{}' completed --auto-reset", wrapper())
        ],
        "repointed and kept"
    );

    let generic = format!("'{}' completed --auto-reset", wrapper());
    let existing =
        format!(r#"{{"hooks": {{"SessionEnd": [{{"hooks": [{{"type": "command", "command": "{generic}"}}]}}]}}}}"#);
    let (json, _) = merge(Some(&existing));
    assert_eq!(
        commands(&json, "SessionEnd"),
        [generic, restore("claude end")],
        "left on the generic wrapper"
    );
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

/// A full current install's six events, every command through `dir`'s adapter, and a user's own event.
fn adapter_settings(dir: &str) -> String {
    let a = format!("{dir}/{CLAUDE_WRAPPER_NAME}");
    format!(
        r#"{{"model": "opus", "hooks": {{
          "UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "'{a}' active --blink"}}]}}],
          "PostToolUse": [{{"hooks": [{{"type": "command", "command": "'{a}' active --blink"}}]}}],
          "Stop": [{{"hooks": [{{"type": "command", "command": "'{a}' completed --auto-reset"}}]}}],
          "PermissionRequest": [{{"hooks": [{{"type": "command", "command": "'{a}' blocked"}}]}}],
          "Notification": [{{"matcher": "permission_prompt", "hooks": [{{"type": "command", "command": "'{a}' blocked"}}]}}],
          "SessionEnd": [{{"hooks": [{{"type": "command", "command": "'{a}' completed --auto-reset"}}]}}],
          "SessionStart": [{{"hooks": [{{"type": "command", "command": "echo mine"}}]}}]
        }}}}"#
    )
}

#[test]
fn an_install_in_the_legacy_dir_is_repointed_not_doubled() {
    let (json, changed) = merge(Some(&adapter_settings(LEGACY_DIR)));
    assert!(changed);
    assert!(!json.contains(LEGACY_DIR), "{json}");
    let current: Value = serde_json::from_str(&merge(None).0).unwrap();
    for (event, entries) in events(&json) {
        if event == "SessionStart" {
            assert_eq!(
                commands(&json, &event),
                ["echo mine".to_owned(), restore("claude start")],
                "the user's own entry survives beside the adapter's"
            );
            continue;
        }
        assert_eq!(
            entries, current["hooks"][&event],
            "{event}: the entry a fresh install writes"
        );
    }
    assert!(json.contains("\"model\": \"opus\""));
    let (again, changed) = merge(Some(&json));
    assert!(!changed, "a second run changes nothing");
    assert_eq!(again, json);
}

#[test]
fn legacy_entries_beside_current_ones_are_dropped() {
    // the fork reinstalled over this build's install: both sets, every event doubled
    let mut root: Value = serde_json::from_str(&adapter_settings(SCRIPT_DIR)).unwrap();
    let legacy: Value = serde_json::from_str(&adapter_settings(LEGACY_DIR)).unwrap();
    for (event, entries) in legacy["hooks"].as_object().unwrap() {
        // the fork wires no SessionEnd
        if event != "SessionStart" && event != "SessionEnd" {
            root["hooks"][event]
                .as_array_mut()
                .unwrap()
                .extend(entries.as_array().unwrap().clone());
        }
    }
    let (json, changed) = merge(Some(&root.to_string()));
    assert!(changed);
    assert!(!json.contains(LEGACY_DIR), "{json}");
    for (event, _, state) in CLAUDE_HOOKS {
        assert_eq!(
            status_commands(&json, event),
            [format!("'{}' {state}", adapter())],
            "{event}"
        );
    }
    assert!(!merge(Some(&json)).1);
}

#[test]
fn a_legacy_generic_wrapper_install_lands_on_the_adapter_in_the_new_dir() {
    let legacy = legacy_settings(None).replace(SCRIPT_DIR, LEGACY_DIR);
    assert!(legacy.contains(LEGACY_DIR));
    let (json, changed) = merge(Some(&legacy));
    assert!(changed);
    assert!(!json.contains(LEGACY_DIR));
    let a = adapter();
    assert_eq!(commands(&json, "UserPromptSubmit"), [format!("'{a}' active --blink")]);
    assert_eq!(commands(&json, "Notification"), [format!("'{a}' blocked")]);
    assert_eq!(events(&json)["Notification"][0]["matcher"], "permission_prompt");
    assert!(!merge(Some(&json)).1);
}

#[test]
fn a_users_command_calling_the_legacy_wrapper_is_repointed_and_kept() {
    let mixed = format!("my-notifier.sh && '{LEGACY_DIR}/{WRAPPER_NAME}' active --blink");
    let existing =
        format!(r#"{{"hooks": {{"UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "{mixed}"}}]}}]}}}}"#);
    let (json, changed) = merge(Some(&existing));
    assert!(changed);
    assert_eq!(
        commands(&json, "UserPromptSubmit"),
        [format!("my-notifier.sh && '{}' active --blink", wrapper())],
        "repointed in place and counted as ours: no stock entry beside it"
    );
    assert_eq!(events(&json).len(), 7);
}

#[test]
fn a_users_legacy_command_beside_a_current_entry_is_repointed_not_dropped() {
    // both installers ran (every event has a current entry), then the user chained and edited hooks against the
    // fork's wrapper: only the stock legacy entries go, the user's survive repointed
    let mut root: Value = serde_json::from_str(&adapter_settings(SCRIPT_DIR)).unwrap();
    let chained = format!("my-notifier.sh && '{LEGACY_DIR}/{WRAPPER_NAME}' active --blink");
    let edited = format!("'{LEGACY_DIR}/{CLAUDE_WRAPPER_NAME}' completed --auto-reset; paplay ~/done.oga");
    let stock = format!("'{LEGACY_DIR}/{CLAUDE_WRAPPER_NAME}' blocked");
    for (event, command) in [
        ("UserPromptSubmit", &chained),
        ("Stop", &edited),
        ("PermissionRequest", &stock),
    ] {
        let entry: Value = serde_json::json!({"hooks": [{"type": "command", "command": command}]});
        root["hooks"][event].as_array_mut().unwrap().push(entry);
    }
    let (json, changed) = merge(Some(&root.to_string()));
    assert!(changed);
    assert!(!json.contains(LEGACY_DIR), "{json}");
    let a = adapter();
    assert_eq!(
        commands(&json, "UserPromptSubmit"),
        [
            format!("'{a}' active --blink"),
            format!("my-notifier.sh && '{}' active --blink", wrapper())
        ]
    );
    assert_eq!(
        commands(&json, "Stop"),
        [
            format!("'{a}' completed --auto-reset"),
            format!("'{a}' completed --auto-reset; paplay ~/done.oga")
        ]
    );
    assert_eq!(commands(&json, "PermissionRequest"), [format!("'{a}' blocked")]);
    assert!(!merge(Some(&json)).1);
}

#[test]
fn a_stock_legacy_command_under_another_matcher_event_or_key_is_kept() {
    // a user copied stock commands into their own matcher, event or entry: none of these is what the install
    // generated, so each is repointed and kept beside the current entry
    let mut root: Value = serde_json::from_str(&adapter_settings(SCRIPT_DIR)).unwrap();
    let blocked = format!("'{LEGACY_DIR}/{CLAUDE_WRAPPER_NAME}' blocked");
    let user_entries = [
        (
            "Notification",
            serde_json::json!({"matcher": "idle_prompt", "hooks": [{"type": "command", "command": blocked}]}),
        ),
        (
            "PostToolUse",
            serde_json::json!({"matcher": "ExitPlanMode", "hooks": [{"type": "command", "command": blocked}]}),
        ),
        (
            "Stop",
            serde_json::json!({"hooks": [{"type": "command", "command": blocked}]}),
        ),
        (
            "PermissionRequest",
            serde_json::json!({"hooks": [{"type": "command", "command": blocked, "timeout": 5}]}),
        ),
    ];
    for (event, entry) in &user_entries {
        root["hooks"][*event].as_array_mut().unwrap().push(entry.clone());
    }
    let (json, changed) = merge(Some(&root.to_string()));
    assert!(changed);
    assert!(!json.contains(LEGACY_DIR), "{json}");
    let a = adapter();
    for (event, _) in &user_entries {
        let found = commands(&json, event);
        assert_eq!(found.len(), 2, "{event}: {found:?}");
        assert_eq!(found[1], format!("'{a}' blocked"), "{event}");
    }
    assert_eq!(events(&json)["Notification"][1]["matcher"], "idle_prompt");
    assert_eq!(events(&json)["PermissionRequest"][1]["hooks"][0]["timeout"], 5);
    assert!(!merge(Some(&json)).1);
}

#[test]
fn a_stock_legacy_entry_is_kept_when_only_another_matcher_calls_the_new_dir() {
    // the user's own idle_prompt hook on the new install, the stock permission_prompt one left in the fork's dir: the
    // stock one is repointed and kept, not dropped for a current entry that covers other notifications
    let idle = format!("'{SCRIPT_DIR}/{CLAUDE_WRAPPER_NAME}' blocked");
    let stock = format!("'{LEGACY_DIR}/{CLAUDE_WRAPPER_NAME}' blocked");
    let existing = serde_json::json!({"hooks": {"Notification": [
        {"matcher": "idle_prompt", "hooks": [{"type": "command", "command": idle}]},
        {"matcher": "permission_prompt", "hooks": [{"type": "command", "command": stock}]},
    ]}});
    let (json, changed) = merge(Some(&existing.to_string()));
    assert!(changed);
    assert!(!json.contains(LEGACY_DIR), "{json}");
    let notification = &events(&json)["Notification"];
    let matchers: Vec<&str> = notification
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["matcher"].as_str().unwrap())
        .collect();
    assert_eq!(matchers, ["idle_prompt", "permission_prompt"]);
    assert_eq!(
        notification[1]["hooks"][0]["command"],
        format!("'{}' blocked", adapter())
    );
    assert!(!merge(Some(&json)).1);
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
            merge_claude_settings(Some(malformed), SCRIPT_DIR, &[LEGACY_DIR]),
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
        assert_eq!(events(&json).len(), 7);
    }
}

#[test]
fn the_codex_block_maps_every_event_to_the_adapter() {
    let block = codex_hooks_block(SCRIPT_DIR);
    let hook = format!("{SCRIPT_DIR}/ngterm-codex-status.sh");
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
    assert!(block.contains("command = \"'/home/my name/agent-status/ngterm-codex-status.sh' session-start\""));
    let block = codex_hooks_block("/home/O'Brien/agent-status");
    assert!(block.contains("'/home/O'\\\\''Brien/agent-status/ngterm-codex-status.sh' session-start"));
    let parsed: toml::Table = format!("{block}\n").parse().unwrap();
    let command = parsed["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(command, "'/home/O'\\''Brien/agent-status/ngterm-codex-status.sh' stop");
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
    let hook = format!("{SCRIPT_DIR}/ngterm-codex-status.sh");
    for (_, action) in CODEX_HOOKS {
        assert!(contents.contains(&format!("'{hook}' {action}")), "{action}");
    }
    assert!(!contents.contains(&format!("'{legacy}' blocked")));
    assert!(contents.contains(
        "command = \"'/home/me/.config/ngterm/agent-status/ngterm-codex-status.sh' stop\"\n\n[hooks.state]\n"
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
    let legacy = "notify = [\"/home/me/.config/ngterm/agent-status/codex-notify.sh\"]\n";
    let contents = merged(merge_codex_config(legacy, SCRIPT_DIR));
    assert!(!contents.contains("codex-notify.sh"));
    assert!(contents.contains("[[hooks.Stop]]"));

    let own = "# my codex config\nmodel = \"gpt-5\"\nnotify = [\"/home/me/my-own-notify.sh\"]";
    let contents = merged(merge_codex_config(own, SCRIPT_DIR));
    assert!(contents.starts_with(own));
    assert!(contents.contains(&format!("my-own-notify.sh\"]\n\n{RC_MARKER_BEGIN}")));

    let multiline =
        "notify = [\"/home/me/.config/ngterm/agent-status/codex-notify.sh\",\n  \"--flag\"]\nmodel = \"x\"\n";
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
    assert!(fresh.contains("source '/home/me/.config/ngterm/agent-status/shell/integration.fish'"));
    let (unterminated, _) = append_shell_rc("export FOO=1", SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    assert!(unterminated.starts_with("export FOO=1\n\n# >>>"));
}

#[test]
fn codex_merge_repoints_a_legacy_dir_block() {
    let first = merged(merge_codex_config("model = \"gpt-5\"\n", LEGACY_DIR));
    assert!(first.contains(LEGACY_DIR));
    let contents = merged(merge_codex_config(&first, SCRIPT_DIR));
    assert!(!contents.contains(LEGACY_DIR), "{contents}");
    assert_eq!(contents, merged(merge_codex_config("model = \"gpt-5\"\n", SCRIPT_DIR)));
    assert_eq!(merge_codex_config(&contents, SCRIPT_DIR), CodexMerge::Unchanged);
}

#[test]
fn a_legacy_dir_rc_block_is_rewritten_in_place() {
    for script in [INTEGRATION_RELATIVE_PATH, FISH_INTEGRATION_RELATIVE_PATH] {
        let (legacy, _) = append_shell_rc("export FOO=1\n", LEGACY_DIR, script);
        let with_tail = format!("{legacy}alias ll='ls -l'\n");
        let (contents, changed) = append_shell_rc(&with_tail, SCRIPT_DIR, script);
        assert!(changed, "{script}");
        assert_eq!(
            contents,
            format!(
                "export FOO=1\n\n{RC_MARKER_BEGIN}\nsource '{SCRIPT_DIR}/{script}'\n{RC_MARKER_END}\nalias ll='ls -l'\n"
            )
        );
        let (again, changed) = append_shell_rc(&contents, SCRIPT_DIR, script);
        assert!(!changed, "{script}: a second run changes nothing");
        assert_eq!(again, contents);
    }
}

#[test]
fn an_rc_block_naming_no_integration_script_is_left_alone() {
    for foreign in [
        format!("{RC_MARKER_BEGIN}\nsource ~/my-own.sh\n{RC_MARKER_END}\n"),
        format!("{RC_MARKER_BEGIN}\nsource '{LEGACY_DIR}/shell/integration.sh'\n"),
    ] {
        assert_eq!(
            append_shell_rc(&foreign, SCRIPT_DIR, INTEGRATION_RELATIVE_PATH),
            (foreign.clone(), false)
        );
    }
}

#[test]
fn pi_paths_and_ownership() {
    assert_eq!(
        pi_extension_path("/home/me"),
        "/home/me/.pi/agent/extensions/ngterm-status.ts"
    );
    assert!(may_overwrite_pi_extension(false, None));
    assert!(may_overwrite_pi_extension(
        true,
        Some("// ngterm-pi-status-extension\nexport default () => {}\n")
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

const WRAPPER_STUB: &str = "#!/usr/bin/env bash\nset -u\n\"${NGTERMCTL:-ngtermctl}\" session status \"$1\"";

#[test]
fn baking_inserts_the_block_after_the_shebang_with_a_path_fallback() {
    let baked = bake_ngtermctl_path(WRAPPER_STUB, "/opt/ngterm/bin/ngtermctl");
    let lines: Vec<&str> = baked.split('\n').collect();
    assert_eq!(lines[0], "#!/usr/bin/env bash");
    assert_eq!(lines[1], NGTERMCTL_MARKER);
    assert!(lines.contains(&"  NGTERMCTL='/opt/ngterm/bin/ngtermctl'"));
    assert!(baked.contains(r#"  [ -x "$NGTERMCTL" ] || NGTERMCTL="$(command -v ngtermctl || true)""#));
    assert!(baked.ends_with("set -u\n\"${NGTERMCTL:-ngtermctl}\" session status \"$1\""));
}

#[test]
fn rebaking_replaces_the_block_instead_of_stacking_it() {
    let first = bake_ngtermctl_path(WRAPPER_STUB, "/tmp/old/ngtermctl");
    let moved = bake_ngtermctl_path(&first, "/opt/ngterm/bin/ngtermctl");
    assert_eq!(moved.matches(NGTERMCTL_MARKER).count(), 1);
    assert!(!moved.contains("/tmp/old"));
    assert_eq!(moved, bake_ngtermctl_path(WRAPPER_STUB, "/opt/ngterm/bin/ngtermctl"));
}

/// `text` with the markers and wrapper names an install from before the rename wrote.
fn before_the_rename(text: &str) -> String {
    text.replace(RC_MARKER_BEGIN, LEGACY_RC_MARKER_BEGIN)
        .replace(RC_MARKER_END, LEGACY_RC_MARKER_END)
        .replace(CLAUDE_WRAPPER_NAME, LEGACY_CLAUDE_WRAPPER_NAME)
        .replace(CODEX_WRAPPER_NAME, LEGACY_CODEX_WRAPPER_NAME)
        .replace(WRAPPER_NAME, LEGACY_WRAPPER_NAME)
}

#[test]
fn an_rc_block_from_before_the_rename_takes_the_current_markers() {
    for dir in [SCRIPT_DIR, LEGACY_DIR] {
        for script in [INTEGRATION_RELATIVE_PATH, FISH_INTEGRATION_RELATIVE_PATH] {
            let (current, _) = append_shell_rc("export FOO=1\n", SCRIPT_DIR, script);
            let (old, _) = append_shell_rc("export FOO=1\n", dir, script);
            let old = format!("{}alias ll='ls -l'\n", before_the_rename(&old));
            let (contents, changed) = append_shell_rc(&old, SCRIPT_DIR, script);
            assert!(changed, "{dir} {script}");
            assert_eq!(contents, format!("{current}alias ll='ls -l'\n"), "{dir} {script}");
            assert_eq!(
                append_shell_rc(&contents, SCRIPT_DIR, script),
                (contents.clone(), false)
            );
        }
    }
}

#[test]
fn an_rc_block_from_before_the_rename_beside_a_current_one_is_dropped() {
    let (current, _) = append_shell_rc("", SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    let old = before_the_rename(&current);
    let (contents, changed) = append_shell_rc(&format!("{old}{current}"), SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    assert!(changed);
    assert_eq!(contents, current);
}

#[test]
fn an_old_marker_block_naming_no_integration_script_is_left_alone() {
    let foreign = format!("{LEGACY_RC_MARKER_BEGIN}\nsource ~/my-own.sh\n{LEGACY_RC_MARKER_END}\n");
    let (contents, changed) = append_shell_rc(&foreign, SCRIPT_DIR, INTEGRATION_RELATIVE_PATH);
    assert!(changed, "the current block is appended");
    assert!(contents.starts_with(&foreign), "{contents}");
    assert_eq!(contents.matches(RC_MARKER_BEGIN).count(), 1);
    let codex = format!("{LEGACY_RC_MARKER_BEGIN}\n# user content\nmodel = \"gpt-5\"\n{LEGACY_RC_MARKER_END}\n");
    assert!(merged(merge_codex_config(&codex, SCRIPT_DIR)).starts_with(&codex));
}

#[test]
fn a_codex_block_from_before_the_rename_is_taken_over_with_its_trust_state() {
    for dir in [SCRIPT_DIR, LEGACY_DIR] {
        let current = merged(merge_codex_config("model = \"gpt-5\"\n", SCRIPT_DIR));
        let trust = "\n[hooks.state]\n\n[hooks.state.\"/home/me/.codex/config.toml:stop:0:0\"]\ntrusted_hash = \"sha256:kept\"\n";
        let with_trust = |text: &str, end: &str| text.replace(&format!("{end}\n"), &format!("{trust}{end}\n"));
        let old = before_the_rename(&merged(merge_codex_config("model = \"gpt-5\"\n", dir)));
        let contents = merged(merge_codex_config(&with_trust(&old, LEGACY_RC_MARKER_END), SCRIPT_DIR));
        assert_eq!(contents, with_trust(&current, RC_MARKER_END), "{dir}");
        assert_eq!(merge_codex_config(&contents, SCRIPT_DIR), CodexMerge::Unchanged);
    }
}

#[test]
fn claude_entries_from_before_the_rename_move_onto_the_current_wrappers() {
    for dir in [SCRIPT_DIR, LEGACY_DIR] {
        let old = before_the_rename(&adapter_settings(dir));
        let (json, changed) = merge(Some(&old));
        assert!(changed, "{dir}");
        assert!(!json.contains("agterm-"), "{json}");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        let current: Value = serde_json::from_str(&merge(Some(&adapter_settings(SCRIPT_DIR))).0).unwrap();
        assert_eq!(parsed, current, "{dir}: what an install of today's names holds");
        assert_eq!(merge(Some(&json)), (json.clone(), false));
    }
    // a stock old entry beside a current one is dropped, a user's command naming the old wrapper is repointed
    let a = adapter();
    let old = format!("{SCRIPT_DIR}/{LEGACY_CLAUDE_WRAPPER_NAME}");
    let settings = format!(
        r#"{{"hooks": {{
          "Stop": [{{"hooks": [{{"type": "command", "command": "'{old}' completed --auto-reset"}}]}},
                   {{"hooks": [{{"type": "command", "command": "'{a}' completed --auto-reset"}}]}}],
          "SessionStart": [{{"hooks": [{{"type": "command", "command": "'{old}' idle; echo mine"}}]}}]
        }}}}"#
    );
    let (json, _) = merge(Some(&settings));
    assert_eq!(commands(&json, "Stop"), [format!("'{a}' completed --auto-reset")]);
    assert_eq!(
        commands(&json, "SessionStart"),
        [format!("'{a}' idle; echo mine"), restore("claude start")]
    );
}

#[test]
fn the_old_pi_extension_path() {
    assert_eq!(
        legacy_pi_extension_path("/home/me"),
        "/home/me/.pi/agent/extensions/agterm-status.ts"
    );
}

/// The restore adapter is probed on its own: a status hook in `SessionEnd` does not stand in for it, a user's own
/// entry calling it (customized) does, and the user's own `SessionStart` hooks stay beside it.
#[test]
fn the_restore_hooks_are_added_once_beside_the_users_own() {
    let a = adapter();
    let customized = format!("{} --verbose", restore("claude start"));
    let existing = format!(
        r#"{{"hooks": {{
          "SessionStart": [{{"hooks": [{{"type": "command", "command": "echo mine"}}]}},
                           {{"matcher": "startup", "hooks": [{{"type": "command", "command": "{customized}"}}]}}],
          "SessionEnd": [{{"hooks": [{{"type": "command", "command": "'{a}' completed --auto-reset"}}]}}]
        }}}}"#
    );
    let (json, changed) = merge(Some(&existing));
    assert!(changed);
    assert_eq!(commands(&json, "SessionStart"), ["echo mine".to_owned(), customized]);
    assert_eq!(
        commands(&json, "SessionEnd"),
        [format!("'{a}' completed --auto-reset"), restore("claude end")]
    );
    assert_eq!(merge(Some(&json)), (json.clone(), false));
    for malformed in [
        r#"{"hooks": {"SessionStart": "nope"}}"#,
        r#"{"hooks": {"SessionStart": [1]}}"#,
    ] {
        assert_eq!(
            merge_claude_settings(Some(malformed), SCRIPT_DIR, &[LEGACY_DIR]),
            Err(MalformedSettings),
            "{malformed}"
        );
    }
}
