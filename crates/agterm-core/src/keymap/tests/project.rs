//! Ported from upstream's `ControlKeymapTests` (the request/response envelope round trips belong to the protocol
//! layer; the payload's own JSON round trip is checked here).

use agterm_proto::keymap::{ControlKeymap, ControlKeymapAction, ControlKeymapMenuItem};
use serde_json::json;

use super::*;

use BuiltinAction as A;

const PATH: &str = "/tmp/keymap.conf";

fn projected(keymap: &Keymap) -> ControlKeymap {
    project(keymap, &[], PATH, None)
}

fn row<'a>(payload: &'a ControlKeymap, action: &str) -> &'a ControlKeymapAction {
    payload
        .actions
        .iter()
        .find(|row| row.action == action)
        .unwrap_or_else(|| panic!("no row for {action}"))
}

fn parsed_clean(text: &str) -> Keymap {
    let (keymap, diagnostics) = parse(text);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    keymap
}

fn round_trip(payload: &ControlKeymap) -> ControlKeymap {
    let encoded = serde_json::to_string(payload).unwrap_or_default();
    serde_json::from_str(&encoded).unwrap_or_else(|error| panic!("{error}: {encoded}"))
}

#[test]
fn every_builtin_action_is_projected_in_declaration_order() {
    let payload = projected(&keymap_with_overrides([]));
    let names: Vec<&str> = payload.actions.iter().map(|row| row.action.as_str()).collect();
    assert_eq!(names, BuiltinAction::all().map(BuiltinAction::name).collect::<Vec<_>>());
    assert_eq!(payload.path, PATH);
}

#[test]
fn shipped_defaults_are_reported_unmarked() {
    let payload = projected(&keymap_with_overrides([]));
    let close = row(&payload, "close_session");
    assert_eq!(close.chord.as_deref(), Some("cmd+w"));
    assert_eq!(close.overridden, None, "a shipped default is not an override");
}

#[test]
fn an_overridden_action_is_marked_with_its_new_chord() {
    let payload = projected(&keymap_with_overrides([(A::CloseSession, chord(CMD, "e"))]));
    let close = row(&payload, "close_session");
    assert_eq!(close.chord.as_deref(), Some("cmd+e"));
    assert_eq!(close.overridden, Some(true));
}

#[test]
fn keyless_actions_appear_with_no_chord() {
    let payload = projected(&keymap_with_overrides([]));
    let keyless: Vec<BuiltinAction> = BuiltinAction::all()
        .filter(|&action| default_chord(action).is_none())
        .collect();
    assert!(!keyless.is_empty());
    for action in keyless {
        assert_eq!(row(&payload, action.name()).chord, None);
    }
}

#[test]
fn mapping_an_action_to_its_own_default_is_not_an_override() {
    let payload = projected(&parsed_clean("map cmd+w close_session\n"));
    let close = row(&payload, "close_session");
    assert_eq!(close.chord.as_deref(), Some("cmd+w"));
    assert_eq!(close.overridden, None);
}

#[test]
fn mapping_a_keyless_action_counts_as_an_override() {
    let payload = projected(&keymap_with_overrides([(A::RenameWindow, chord(CMD, "y"))]));
    let rename = row(&payload, "rename_window");
    assert_eq!(rename.chord.as_deref(), Some("cmd+y"));
    assert_eq!(rename.overridden, Some(true));
}

#[test]
fn custom_commands_are_carried_and_palette_only_ones_have_no_shortcut() {
    let (keymap, diagnostics) = parse("command \"Bound\" cmd+shift+e echo hi\ncommand \"Palette Only\" echo hi");
    let payload = project(&keymap, &diagnostics, PATH, None);
    assert_eq!(payload.commands.len(), 2);
    assert_eq!(payload.commands[0].shortcut.as_deref(), Some("cmd+shift+e"));
    assert_eq!(
        payload.commands[1].shortcut, None,
        "an empty shortcut reads as palette-only"
    );
}

#[test]
fn parse_diagnostics_are_carried() {
    let (keymap, diagnostics) = parse("map cmd+shift+d not_an_action\n");
    let payload = project(&keymap, &diagnostics, PATH, None);
    assert_eq!(payload.diagnostics.len(), diagnostics.len());
    assert_eq!(payload.diagnostics[0].line, 1);
    assert_eq!(payload.diagnostics[0].message, diagnostics[0].message);
}

#[test]
fn alternatives_are_reported_beside_the_menu_chord_in_line_order() {
    let payload = projected(&parsed_clean("map cmd+t|ctrl+space>s|cmd+ctrl+y toggle_split"));
    let split = row(&payload, "toggle_split");
    assert_eq!(
        split.chord.as_deref(),
        Some("cmd+t"),
        "chord stays the menu key equivalent alone"
    );
    assert_eq!(
        split.alternates,
        Some(vec!["ctrl+space>s".to_owned(), "ctrl+cmd+y".to_owned()])
    );
    assert_eq!(split.overridden, Some(true));
}

#[test]
fn an_unbound_action_reports_its_alternatives_only() {
    let payload = projected(&parsed_clean("map ctrl+space>s toggle_split"));
    let split = row(&payload, "toggle_split");
    assert_eq!(split.chord, None);
    assert_eq!(split.alternates, Some(vec!["ctrl+space>s".to_owned()]));
    assert_eq!(split.overridden, Some(true));
}

#[test]
fn alternates_are_omitted_for_an_action_with_none() {
    let payload = projected(&keymap_with_overrides([]));
    assert!(payload.actions.iter().all(|row| row.alternates.is_none()));
    assert!(
        !serde_json::to_string(&payload)
            .unwrap_or_default()
            .contains("alternates")
    );
}

#[test]
fn alternates_round_trip_over_the_wire() {
    let payload = projected(&parse("map cmd+t|ctrl+space>s toggle_split").0);
    let back = round_trip(&payload);
    assert_eq!(back, payload);
    assert_eq!(
        row(&back, "toggle_split").alternates,
        Some(vec!["ctrl+space>s".to_owned()])
    );
}

#[test]
fn repeat_is_reported_only_where_a_sequence_survived() {
    let keymap = parsed_clean(
        "map cmd+opt+down|ctrl+a>ctrl+l --repeat next_session\n\
         map cmd+shift+e --repeat toggle_split\n\
         map f5|f6 --repeat previous_session\n\
         command \"Grow\" ctrl+a>x --repeat ./grow.sh\n\
         command \"x\" cmd+g --repeat ./s",
    );
    let back = round_trip(&projected(&keymap));
    assert_eq!(row(&back, "next_session").repeats, Some(true));
    assert_eq!(
        row(&back, "toggle_split").repeats,
        None,
        "a menu chord has nothing to repeat"
    );
    assert_eq!(
        row(&back, "previous_session").repeats,
        None,
        "nor does a single-chord alternative"
    );
    let repeats: Vec<bool> = back.commands.iter().map(|command| command.repeats).collect();
    assert_eq!(repeats, [true, false]);
}

#[test]
fn a_pipe_free_keymap_projects_with_no_alternates_at_all() {
    let (keymap, diagnostics) = parse(PIPE_FREE_KEYMAP_FIXTURE);
    let payload = project(&keymap, &diagnostics, PATH, None);
    assert!(payload.actions.iter().all(|row| row.alternates.is_none()));
    for (action, chord) in [
        ("toggle_split", Some("cmd+shift+e")),
        ("toggle_sidebar", Some("t")),
        ("focus_left_pane", Some("ctrl+cmd+left")),
        ("close_session", Some("cmd+w")),
        ("new_session", Some("cmd+n")),
        ("first_session", None),
    ] {
        assert_eq!(row(&payload, action).chord.as_deref(), chord, "{action}");
    }
    assert!(
        !serde_json::to_string(&payload)
            .unwrap_or_default()
            .contains("alternates")
    );
}

#[test]
fn the_menu_is_omitted_when_the_caller_supplies_none_and_round_trips_when_it_does() {
    assert_eq!(projected(&keymap_with_overrides([])).menu, None);
    let menu = vec![ControlKeymapMenuItem {
        menu: "File".to_owned(),
        title: "Close".to_owned(),
        chord: "cmd+w".to_owned(),
        selector: Some("close_session".to_owned()),
        enabled: None,
    }];
    let keymap = keymap_with_overrides([(A::CloseSession, chord(CMD, "e"))]);
    let payload = project(&keymap, &[], PATH, Some(menu));
    let back = round_trip(&payload);
    assert_eq!(back, payload);
    assert_eq!(row(&back, "close_session").chord.as_deref(), Some("cmd+e"));
    assert_eq!(back.menu.as_ref().map(|menu| menu[0].chord.as_str()), Some("cmd+w"));
}

#[test]
fn the_conformance_keymap_projects_upstreams_payload() {
    let text = "# conformance keymap\nmap ctrl+shift+f12|ctrl+shift+f10 new_session\ncommand \"plain\" true\n\
                command \"keyed\" ctrl+shift+f11 --error-hud --error-position top-left --error-pane right true\n\
                bogus line here\n";
    let (keymap, diagnostics) = parse(text);
    let payload = serde_json::to_value(project(&keymap, &diagnostics, PATH, None)).unwrap_or_default();
    let new_session = payload["actions"]
        .as_array()
        .and_then(|actions| actions.iter().find(|row| row["action"] == "new_session"))
        .cloned();
    assert_eq!(
        new_session,
        Some(
            json!({"action": "new_session", "chord": "ctrl+shift+f12", "alternates": ["ctrl+shift+f10"], "overridden": true})
        )
    );
    assert_eq!(
        payload["commands"],
        json!([
            {"name": "plain", "errorHud": false, "errorPosition": "center", "repeats": false},
            {"name": "keyed", "shortcut": "ctrl+shift+f11", "errorHud": true, "errorPosition": "top-left",
             "errorPane": "right", "repeats": false},
        ])
    );
    assert_eq!(
        payload["diagnostics"],
        json!([{"line": 5, "message": "unknown verb 'bogus'"}])
    );
    assert_eq!(payload["path"], PATH);
    assert!(payload.get("menu").is_none());
}
