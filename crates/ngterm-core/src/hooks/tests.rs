//! The parser after upstream's `HooksTests`, the loader and starter after `SettingsModel.loadHooks` /
//! `ensureStarterHooks`, and the child's environment and stdin after `HookProcessRunnerTests`.

use ngterm_proto::{ControlEvent, EventKind, EventPayload};
use strum::IntoEnumIterator;

use super::*;

fn diagnostic(line: usize, message: &str) -> KeymapDiagnostic {
    KeymapDiagnostic::new(line, message)
}

fn commands(hooks: &Hooks) -> Vec<&str> {
    hooks.entries.iter().map(HookEntry::command).collect()
}

fn kinds(hooks: &Hooks) -> Vec<EventKind> {
    hooks.entries.iter().map(HookEntry::kind).collect()
}

fn lines(hooks: &Hooks) -> Vec<usize> {
    hooks.entries.iter().map(|entry| entry.line).collect()
}

#[test]
fn every_ring_kind_parses_with_its_remainder() {
    let text = EventKind::iter()
        .map(|kind| format!("on {kind} echo {kind}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (hooks, diagnostics) = parse_hooks_conf(&text);
    assert!(diagnostics.is_empty());
    assert_eq!(kinds(&hooks), EventKind::iter().collect::<Vec<_>>());
    assert_eq!(
        commands(&hooks),
        EventKind::iter().map(|kind| format!("echo {kind}")).collect::<Vec<_>>()
    );
    assert_eq!(lines(&hooks), (1..=EventKind::iter().count()).collect::<Vec<_>>());
}

#[test]
fn several_lines_per_kind_keep_file_order() {
    let (hooks, diagnostics) = parse_hooks_conf("on status ~/a.sh\non session.closed ~/c.sh\non status ~/b.sh");
    assert!(diagnostics.is_empty());
    assert_eq!(commands(&hooks), ["~/a.sh", "~/c.sh", "~/b.sh"]);
    assert_eq!(
        kinds(&hooks),
        [EventKind::Status, EventKind::SessionClosed, EventKind::Status]
    );
}

#[test]
fn blank_and_whole_line_comments_are_ignored_and_crlf_is_normalized() {
    let text = "# header\r\n\r\n   # indented comment\r\non status echo a\r\n\r\non notify echo b\r\n";
    let (hooks, diagnostics) = parse_hooks_conf(text);
    assert!(diagnostics.is_empty());
    assert_eq!(commands(&hooks), ["echo a", "echo b"]);
    assert_eq!(lines(&hooks), [4, 6]);
}

#[test]
fn a_lone_cr_ends_a_line_too() {
    let (hooks, diagnostics) = parse_hooks_conf("on status a\ron notify b");
    assert!(diagnostics.is_empty());
    assert_eq!(commands(&hooks), ["a", "b"]);
    assert_eq!(lines(&hooks), [1, 2]);
}

#[test]
fn shell_remainder_is_preserved_verbatim() {
    for command in [
        "echo 'fix #42'",
        "echo \"a \\\" b\"",
        "ls | grep x",
        "echo hi >> ~/log.txt",
        "echo $(date) $HOME",
        "printf '%s\\n' a",
        "echo  two  spaces",
        "~/bin/x.sh",
        "echo hi # not a comment",
    ] {
        let (hooks, diagnostics) = parse_hooks_conf(&format!("on status \t {command} \t "));
        assert!(diagnostics.is_empty(), "{command}");
        assert_eq!(commands(&hooks), [command]);
    }
}

#[test]
fn trimming_is_foundations_whitespaces_set() {
    // a no-break space trims like a space; a form feed is whitespace to the word split but never trimmed
    let (hooks, diagnostics) = parse_hooks_conf("\u{a0}on\u{a0}status\u{a0}echo x\u{a0}");
    assert!(diagnostics.is_empty());
    assert_eq!(commands(&hooks), ["echo x"]);
    let (hooks, diagnostics) = parse_hooks_conf("\u{0C}on status echo x");
    assert!(hooks.entries.is_empty());
    assert_eq!(diagnostics, [diagnostic(1, "unknown verb ''")]);
}

#[test]
fn identity_is_kind_plus_command_regardless_of_spacing_and_line() {
    let (hooks, diagnostics) = parse_hooks_conf(
        "on   status\t   echo x\n# a comment between them changes the line number, never the identity\n\
         on status echo x\non notify echo x",
    );
    assert_eq!(commands(&hooks), ["echo x", "echo x"]);
    assert_eq!(kinds(&hooks), [EventKind::Status, EventKind::Notify]);
    assert_eq!(hooks.entries[0].line, 1);
    assert_eq!(
        diagnostics,
        [diagnostic(
            3,
            "hook 'on status echo x' is already defined; hook skipped"
        )]
    );
    let identity = HookIdentity {
        kind: EventKind::Status,
        command: "echo x".to_owned(),
    };
    assert_eq!(hooks.entries[0].identity, identity);
    let moved = HookEntry {
        identity: hooks.entries[0].identity.clone(),
        line: 99,
    };
    assert_eq!(moved.identity, hooks.entries[0].identity);
}

#[test]
fn malformed_lines_are_diagnosed_and_later_lines_still_parse() {
    let (hooks, diagnostics) =
        parse_hooks_conf("map cmd+a toggle_split\non\non bogus echo x\non status\non status \t\non notify echo ok");
    assert_eq!(commands(&hooks), ["echo ok"]);
    assert_eq!(lines(&hooks), [6]);
    assert_eq!(
        diagnostics,
        [
            diagnostic(1, "unknown verb 'map'"),
            diagnostic(2, "on requires an event kind"),
            diagnostic(3, "unknown event kind 'bogus'"),
            diagnostic(4, "hook for 'status' has no shell line"),
            diagnostic(5, "hook for 'status' has no shell line"),
        ]
    );
}

#[test]
fn event_kinds_match_exactly() {
    let (hooks, diagnostics) = parse_hooks_conf("on Status echo x\non session echo y");
    assert!(hooks.entries.is_empty());
    assert_eq!(
        diagnostics,
        [
            diagnostic(1, "unknown event kind 'Status'"),
            diagnostic(2, "unknown event kind 'session'"),
        ]
    );
}

#[test]
fn empty_text_parses_to_nothing() {
    let (hooks, diagnostics) = parse_hooks_conf("");
    assert!(hooks.entries.is_empty());
    assert!(diagnostics.is_empty());
}

/// The conformance suite's `hooks.conf`: every diagnostic kind, and a duplicate that differs only in spacing.
#[test]
fn the_conformance_file_parses_to_one_hook_and_four_diagnostics() {
    let text = "# conformance hooks\non session.created echo created > /dev/null\non session.renamed true\n\
                off status true\non notify\non session.created   echo created > /dev/null  \n";
    let (hooks, diagnostics) = parse_hooks_conf(text);
    assert_eq!(commands(&hooks), ["echo created > /dev/null"]);
    assert_eq!(kinds(&hooks), [EventKind::SessionCreated]);
    assert_eq!(lines(&hooks), [2]);
    assert_eq!(
        diagnostics,
        [
            diagnostic(3, "unknown event kind 'session.renamed'"),
            diagnostic(4, "unknown verb 'off'"),
            diagnostic(5, "hook for 'notify' has no shell line"),
            diagnostic(
                6,
                "hook 'on session.created echo created > /dev/null' is already defined; hook skipped"
            ),
        ]
    );
}

#[test]
fn hooks_conf_sits_in_the_config_directory() {
    assert_eq!(
        hooks_path(Path::new("/cfg/ngterm")),
        Path::new("/cfg/ngterm/hooks.conf")
    );
}

#[test]
fn a_missing_file_loads_as_no_hooks_and_no_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let (hooks, diagnostics) = load_hooks(&hooks_path(dir.path()));
    assert!(hooks.entries.is_empty());
    assert!(diagnostics.is_empty());
}

#[test]
fn an_existing_file_loads_and_parses() {
    let dir = tempfile::tempdir().unwrap();
    let path = hooks_path(dir.path());
    fs::write(&path, "on status echo a\non bogus x\n").unwrap();
    let (hooks, diagnostics) = load_hooks(&path);
    assert_eq!(commands(&hooks), ["echo a"]);
    assert_eq!(diagnostics, [diagnostic(2, "unknown event kind 'bogus'")]);
}

#[test]
fn an_unreadable_file_never_loads_as_clean() {
    let dir = tempfile::tempdir().unwrap();
    let path = hooks_path(dir.path());
    fs::write(&path, b"on status echo \xff\xfe\n").unwrap();
    let (hooks, diagnostics) = load_hooks(&path);
    assert!(hooks.entries.is_empty());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line, 0);
    assert!(diagnostics[0].message.starts_with("could not read hooks.conf: "));

    // a directory where the file should be reads as unreadable too
    let dir = tempfile::tempdir().unwrap();
    let path = hooks_path(dir.path());
    fs::create_dir(&path).unwrap();
    let (hooks, diagnostics) = load_hooks(&path);
    assert!(hooks.entries.is_empty());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line, 0);
    assert!(
        !diagnostics[0].message.contains("os error"),
        "{}",
        diagnostics[0].message
    );
}

#[test]
fn the_starter_lists_every_kind_and_parses_to_nothing() {
    let text = starter_hooks_conf();
    let kinds = EventKind::iter()
        .map(EventKind::wire_name)
        .collect::<Vec<_>>()
        .join(", ");
    assert!(text.contains(&format!("# Kinds: {kinds}.\n")));
    assert!(text.contains("status, notify, session.created, session.closed, session.selected, tree.changed"));
    assert!(text.starts_with("# ngterm hooks — run a shell line when a control event fires."));
    assert!(text.ends_with("canberra-gtk-play -i dialog-warning\n"));
    assert!(text.lines().all(|line| line.is_empty() || line.starts_with('#')));
    let (hooks, diagnostics) = parse_hooks_conf(&text);
    assert!(hooks.entries.is_empty());
    assert!(diagnostics.is_empty());
}

#[test]
fn the_starter_is_written_with_its_directory_only_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = hooks_path(&dir.path().join("nested").join("ngterm"));
    ensure_starter_hooks(&path);
    assert_eq!(fs::read_to_string(&path).unwrap(), starter_hooks_conf());

    fs::write(&path, "on status mine\n").unwrap();
    ensure_starter_hooks(&path);
    assert_eq!(fs::read_to_string(&path).unwrap(), "on status mine\n");
}

#[test]
fn a_failed_starter_write_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, "").unwrap();
    let path = hooks_path(&blocker);
    ensure_starter_hooks(&path);
    assert!(!path.exists());
}

fn full_event() -> ControlEvent {
    ControlEvent {
        seq: 7,
        ts: 1_783_969_641.38,
        kind: EventKind::Status,
        window: Some("WIN".to_owned()),
        workspace: Some("WORK".to_owned()),
        session: Some("SESS".to_owned()),
        payload: EventPayload {
            status: Some("blocked".to_owned()),
            host: Some("box".to_owned()),
            ..EventPayload::default()
        },
    }
}

#[test]
fn the_environment_carries_every_variable_from_the_event() {
    let environment = hook_environment(&full_event(), "/run/ngterm.sock");
    let expected = [
        ("AGT_EVENT_KIND", "status"),
        ("AGT_EVENT_STATUS", "blocked"),
        ("AGT_EVENT_HOST", "box"),
        ("AGT_SESSION_ID", "SESS"),
        ("AGT_WORKSPACE_ID", "WORK"),
        ("AGT_WINDOW_ID", "WIN"),
        ("AGT_SOCKET", "/run/ngterm.sock"),
    ];
    assert_eq!(
        environment,
        expected.map(|(key, value)| (key.to_owned(), value.to_owned()))
    );
}

#[test]
fn an_absent_field_sets_its_variable_empty() {
    let event = ControlEvent {
        seq: 1,
        ts: 1.0,
        kind: EventKind::TreeChanged,
        window: Some("WIN".to_owned()),
        workspace: None,
        session: None,
        payload: EventPayload::default(),
    };
    let environment = hook_environment(&event, "");
    let names: Vec<&str> = environment.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        names,
        [
            "AGT_EVENT_KIND",
            "AGT_EVENT_STATUS",
            "AGT_EVENT_HOST",
            "AGT_SESSION_ID",
            "AGT_WORKSPACE_ID",
            "AGT_WINDOW_ID",
            "AGT_SOCKET"
        ]
    );
    let values: Vec<&str> = environment.iter().map(|(_, value)| value.as_str()).collect();
    assert_eq!(values, ["tree.changed", "", "", "", "", "WIN", ""]);
}

#[test]
fn stdin_is_the_events_read_json_and_a_newline() {
    let event = full_event();
    let bytes = hook_stdin(&event).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let line = text.strip_suffix('\n').unwrap();
    assert!(!line.contains('\n'));
    assert_eq!(line, serde_json::to_string(&event).unwrap());
    assert_eq!(serde_json::from_str::<ControlEvent>(line).unwrap(), event);
    assert!(line.starts_with(r#"{"seq":7,"ts":1783969641.38,"kind":"status","window":"WIN""#));
}
