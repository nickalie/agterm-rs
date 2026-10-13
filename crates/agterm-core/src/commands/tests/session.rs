//! `session.new`, `duplicate`, `close`, `select`, `go`, `rename` and `reveal`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Command, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_affected, answered_id, args, core_over, failure, send, wire};
use crate::model::{AgentStatus, WindowLibrary};
use crate::persist::AppSettings;

// MARK: - session.new

#[test]
fn new_appends_a_selected_session_at_home_to_the_current_workspace() {
    let mut fixture = Fixture::new(1);
    let response = fixture.session(SessionCmd::New, None, args(|a| a.name = Some("  alpha  ".into())));
    let id = answered_id(&response).unwrap();
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"ok": true, "result": {"id": wire(id)}})
    );
    assert_eq!(
        fixture.order(fixture.workspace),
        [&fixture.sessions[..], &[id]].concat()
    );
    assert_eq!(fixture.selected(), Some(id));
    let session = fixture.store().session(id).unwrap();
    assert_eq!(session.custom_name.as_deref(), Some("alpha"));
    assert_eq!(session.initial_cwd(), "/home/u");
}

#[test]
fn new_threads_cwd_command_and_wait_and_can_stay_in_the_background() {
    let mut fixture = Fixture::new(0);
    let selected = fixture.selected();
    let response = fixture.session(
        SessionCmd::New,
        None,
        args(|a| {
            a.cwd = Some("/srv/app".into());
            a.command = Some("make test".into());
            a.wait = Some(true);
            a.no_select = Some(true);
        }),
    );
    let id = answered_id(&response).unwrap();
    assert_eq!(fixture.selected(), selected);
    let session = fixture.store().session(id).unwrap();
    assert_eq!(session.initial_cwd(), "/srv/app");
    assert_eq!(session.launch.command.as_deref(), Some("make test"));
    assert!(session.launch.wait);
}

#[test]
fn new_places_next_to_an_anchor_in_the_anchors_workspace() {
    let mut fixture = Fixture::new(0);
    let (workspace, ids) = fixture.add_workspace("anchors", 2);
    let (first, last) = (ids[0], ids[1]);
    let after = fixture.session(SessionCmd::New, None, args(|a| a.after = Some(wire(first))));
    let before = fixture.session(SessionCmd::New, None, args(|a| a.before = Some(wire(first))));
    let (after, before) = (answered_id(&after).unwrap(), answered_id(&before).unwrap());
    assert_eq!(fixture.order(workspace), [before, first, after, last]);
    // `active` anchors on the selection
    let next = fixture.session(SessionCmd::New, None, args(|a| a.after = Some("active".into())));
    let next = answered_id(&next).unwrap();
    assert_eq!(fixture.order(workspace), [before, next, first, after, last]);
}

#[test]
fn new_into_a_workspace_by_name_requires_it_or_creates_it() {
    let mut fixture = Fixture::new(0);
    let missing = fixture.session(
        SessionCmd::New,
        None,
        args(|a| a.workspace_name = Some("servers".into())),
    );
    assert_eq!(
        missing,
        failure(r#"no workspace named "servers" (pass --create-workspace to add it)"#)
    );
    let create = |name: &str| {
        args(|a| {
            a.workspace_name = Some(name.into());
            a.create_workspace = Some(true);
        })
    };
    let created = answered_id(&fixture.session(SessionCmd::New, None, create("servers"))).unwrap();
    let workspace = fixture.store().workspace_named("servers").unwrap().id();
    assert_eq!(fixture.order(workspace), [created]);
    let reused = answered_id(&fixture.session(SessionCmd::New, None, create("  servers  "))).unwrap();
    assert_eq!(fixture.order(workspace), [created, reused]);
    assert_eq!(fixture.store().workspaces().len(), 2);
    let by_name = fixture.session(
        SessionCmd::New,
        None,
        args(|a| a.workspace_name = Some("servers".into())),
    );
    assert_eq!(fixture.order(workspace)[2], answered_id(&by_name).unwrap());
    for blank in ["", "   "] {
        assert_eq!(
            fixture.session(SessionCmd::New, None, create(blank)),
            failure("workspace name must not be blank")
        );
    }
}

#[test]
fn new_into_a_workspace_by_id_or_prefix() {
    let mut fixture = Fixture::new(0);
    let (workspace, _) = fixture.add_workspace("by id", 0);
    let prefix = wire(workspace)[..8].to_lowercase();
    let id = answered_id(&fixture.session(SessionCmd::New, None, args(|a| a.workspace = Some(prefix)))).unwrap();
    assert_eq!(fixture.order(workspace), [id]);
}

#[test]
fn new_rejects_conflicting_arguments_and_unresolved_targets_without_creating() {
    let mut fixture = Fixture::new(1);
    let anchor = wire(fixture.sessions[0]);
    let cases: [(Args, &str); 8] = [
        (
            args(|a| {
                a.after = Some(anchor.clone());
                a.before = Some(anchor.clone());
            }),
            "use either --after or --before, not both",
        ),
        (
            args(|a| {
                a.after = Some(anchor.clone());
                a.workspace = Some("active".into());
            }),
            "session.new takes --after/--before or a workspace, not both",
        ),
        (
            args(|a| {
                a.before = Some(anchor.clone());
                a.workspace_name = Some("x".into());
            }),
            "session.new takes --after/--before or a workspace, not both",
        ),
        (
            args(|a| {
                a.workspace = Some("active".into());
                a.workspace_name = Some("x".into());
            }),
            "use either --workspace or --workspace-name, not both",
        ),
        (
            args(|a| a.create_workspace = Some(true)),
            "--create-workspace requires --workspace-name",
        ),
        (args(|a| a.wait = Some(true)), "--wait requires --command"),
        (args(|a| a.workspace = Some(UNKNOWN.into())), "no such workspace: zzz"),
        (args(|a| a.after = Some(UNKNOWN.into())), "no such session: zzz"),
    ];
    for (request, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::New, None, request),
            failure(error),
            "{error}"
        );
        assert_eq!(fixture.order(fixture.workspace), fixture.sessions);
        assert_eq!(fixture.store().workspaces().len(), 1);
    }
}

#[test]
fn new_in_a_window_that_is_unknown_or_closed_is_refused() {
    let mut library = WindowLibrary::new("/home/u");
    let closed = library.new_window(None);
    library.new_window(None);
    library.close_window(closed);
    let mut core = core_over(library, AppSettings::default());
    let in_window = |window: &str| args(|a| a.window = Some(window.into()));
    assert_eq!(
        send(&mut core, Command::Session(SessionCmd::New), None, in_window(UNKNOWN)),
        failure("no such window: zzz")
    );
    assert_eq!(
        send(
            &mut core,
            Command::Session(SessionCmd::New),
            None,
            in_window(&wire(closed))
        ),
        failure("window not open — window.select it first")
    );
}

#[test]
fn with_every_window_closed_new_acts_on_a_throwaway_empty_window() {
    let mut library = WindowLibrary::new("/home/u");
    let window = library.new_window(None);
    library.close_window(window);
    let mut core = core_over(library, AppSettings::default());
    assert_eq!(
        send(&mut core, Command::Session(SessionCmd::New), None, Args::default()),
        failure("no such workspace: active")
    );
    let created = send(
        &mut core,
        Command::Session(SessionCmd::New),
        None,
        args(|a| {
            a.workspace_name = Some("w".into());
            a.create_workspace = Some(true);
        }),
    );
    assert!(answered_id(&created).is_some());
    assert!(core.library().open_ids().is_empty());
    assert_eq!(
        send(
            &mut core,
            Command::Session(SessionCmd::Go),
            None,
            args(|a| a.to = Some("next".into()))
        ),
        failure("no session to navigate")
    );
}

// MARK: - session.duplicate

#[test]
fn duplicate_inserts_a_plain_copy_after_the_source_in_its_focused_directory() {
    let mut fixture = Fixture::new(1);
    let (source, last) = (fixture.sessions[0], fixture.sessions[1]);
    let store = fixture.store_mut().unwrap();
    store.rename_session(source, "original");
    store.set_flag(true, source);
    store.session_mut(source).unwrap().set_current_cwd("/srv");
    let response = fixture.session(SessionCmd::Duplicate, Some(&wire(source)), Args::default());
    let copy = answered_id(&response).unwrap();
    assert_eq!(fixture.order(fixture.workspace), [source, copy, last]);
    assert_eq!(fixture.selected(), Some(copy));
    let session = fixture.store().session(copy).unwrap();
    assert_eq!(session.initial_cwd(), "/srv");
    assert_eq!(session.custom_name, None);
    assert!(!session.flagged);
}

#[test]
fn duplicate_defaults_to_the_selection_and_refuses_an_unknown_target() {
    let mut fixture = Fixture::new(1);
    let selected = fixture.selected().unwrap();
    let copy = answered_id(&fixture.session(SessionCmd::Duplicate, None, Args::default())).unwrap();
    let order = fixture.order(fixture.workspace);
    let at = order.iter().position(|id| *id == selected).unwrap();
    assert_eq!(order[at + 1], copy);
    assert_eq!(
        fixture.session(SessionCmd::Duplicate, Some(UNKNOWN), Args::default()),
        failure("no such session: zzz")
    );
}

// MARK: - session.close

#[test]
fn close_removes_the_target_reselects_and_answers_its_id() {
    let mut fixture = Fixture::new(1);
    let (keep, closing) = (fixture.sessions[0], fixture.sessions[1]);
    assert_eq!(fixture.selected(), Some(closing));
    let response = fixture.session(SessionCmd::Close, Some(&wire(closing)), Args::default());
    assert_eq!(answered_id(&response), Some(closing));
    assert_eq!(fixture.order(fixture.workspace), [keep]);
    assert_eq!(fixture.selected(), Some(keep));
    assert_eq!(
        fixture.session(SessionCmd::Close, Some(UNKNOWN), Args::default()),
        failure("no such session: zzz")
    );
}

fn targets(ids: &[&str]) -> Args {
    args(|a| a.targets = Some(ids.iter().map(|id| (*id).to_owned()).collect()))
}

#[test]
fn close_batch_deduplicates_and_counts_with_grace_undo_on_or_off() {
    for grace in [None, Some(true), Some(false)] {
        let settings = AppSettings {
            close_grace_undo_enabled: grace,
            ..AppSettings::default()
        };
        let mut fixture = Fixture::with_settings(2, settings);
        let [first, keep, last] = [0, 1, 2].map(|at| wire(fixture.sessions[at]));
        let response = fixture.session(SessionCmd::Close, None, targets(&[&first, &last, &first]));
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({"ok": true, "result": {"affected": 2}}),
            "{grace:?}"
        );
        assert_eq!(
            fixture.order(fixture.workspace),
            [fixture.sessions[1]],
            "{keep} {grace:?}"
        );
    }
}

#[test]
fn close_batch_is_atomic_and_needs_a_target() {
    let mut fixture = Fixture::new(1);
    let ids: Vec<String> = fixture.sessions.iter().map(|id| wire(*id)).collect();
    assert_eq!(
        fixture.session(SessionCmd::Close, None, targets(&[&ids[0], &ids[1], UNKNOWN])),
        failure("no such session: zzz")
    );
    assert_eq!(fixture.order(fixture.workspace), fixture.sessions);
    assert_eq!(
        fixture.session(SessionCmd::Close, None, targets(&[])),
        failure("session.close requires at least one --target")
    );
}

#[test]
fn close_batch_of_one_session_closes_it_and_answers_its_id() {
    let mut fixture = Fixture::new(1);
    let closing = wire(fixture.sessions[0]);
    let prefix = closing[..8].to_lowercase();
    let response = fixture.session(SessionCmd::Close, None, targets(&[&closing, &prefix]));
    assert_eq!(answered_id(&response), Some(fixture.sessions[0]));
    assert_eq!(answered_affected(&response), None);
    assert_eq!(fixture.order(fixture.workspace), [fixture.sessions[1]]);
}

// MARK: - session.select and session.go

#[test]
fn select_moves_the_selection_across_windows_and_workspaces() {
    let mut fixture = Fixture::new(0);
    let (workspace, ids) = fixture.add_workspace("other", 1);
    let first = fixture.sessions[0];
    let response = fixture.session(SessionCmd::Select, Some(&wire(first)), Args::default());
    assert_eq!(answered_id(&response), Some(first));
    assert_eq!(fixture.selected(), Some(first));
    let prefix = wire(ids[0])[..8].to_lowercase();
    assert_eq!(
        answered_id(&fixture.session(SessionCmd::Select, Some(&prefix), Args::default())),
        Some(ids[0])
    );
    assert_eq!(fixture.store().current_workspace_id(), Some(workspace));
    // `active` and an omitted target name the selection
    assert_eq!(
        answered_id(&fixture.session(SessionCmd::Select, Some("active"), Args::default())),
        Some(ids[0])
    );
    for target in [UNKNOWN, ""] {
        assert_eq!(
            fixture.session(SessionCmd::Select, Some(target), Args::default()),
            failure(&format!("no such session: {target}"))
        );
    }
}

#[test]
fn select_scoped_to_a_window_does_not_see_other_windows() {
    let mut fixture = Fixture::new(0);
    let other = fixture.core.library_mut().new_window(None);
    let in_other = fixture
        .core
        .library()
        .store(other)
        .unwrap()
        .selected_session_id()
        .unwrap();
    let response = fixture.session(
        SessionCmd::Select,
        Some(&wire(in_other)),
        args(|a| a.window = Some(wire(fixture.window))),
    );
    assert_eq!(response, failure(&format!("no such session: {}", wire(in_other))));
    // without --window an id resolves in any open window
    let response = fixture.session(SessionCmd::Select, Some(&wire(fixture.sessions[0])), Args::default());
    assert_eq!(answered_id(&response), Some(fixture.sessions[0]));
}

#[test]
fn go_steps_through_the_sessions_and_wraps() {
    let mut fixture = Fixture::new(2);
    let ids = fixture.sessions.clone();
    fixture.store_mut().unwrap().select_session(Some(ids[0]));
    let mut current = 0;
    for direction in ["prev", "next", "next", "last", "next", "first", "prev", "previous"] {
        current = match direction {
            "next" => (current + 1) % ids.len(),
            "first" => 0,
            "last" => ids.len() - 1,
            _ => (current + ids.len() - 1) % ids.len(),
        };
        let response = fixture.session(SessionCmd::Go, None, args(|a| a.to = Some(direction.into())));
        assert_eq!(answered_id(&response), Some(ids[current]), "{direction}");
        assert_eq!(fixture.selected(), Some(ids[current]), "{direction}");
    }
}

#[test]
fn go_attention_steps_skip_sessions_that_do_not_need_it() {
    let mut fixture = Fixture::new(2);
    let ids = fixture.sessions.clone();
    let store = fixture.store_mut().unwrap();
    store.select_session(Some(ids[0]));
    let mut indicator = store.session(ids[2]).unwrap().agent_indicator.clone();
    indicator.status = AgentStatus::Blocked;
    store.set_agent_indicator(indicator, ids[2]);
    let response = fixture.session(SessionCmd::Go, None, args(|a| a.to = Some("next-attention".into())));
    assert_eq!(answered_id(&response), Some(ids[2]));
}

#[test]
fn go_requires_a_direction_and_a_session() {
    let mut fixture = Fixture::new(0);
    for to in [None, Some("sideways".into())] {
        assert_eq!(
            fixture.session(SessionCmd::Go, None, args(|a| a.to = to.clone())),
            failure("session.go requires --to next|prev|first|last|next-attention|prev-attention")
        );
    }
    let only = fixture.sessions[0];
    fixture.store_mut().unwrap().close_session(only);
    assert_eq!(
        fixture.session(SessionCmd::Go, None, args(|a| a.to = Some("next".into()))),
        failure("no session to navigate")
    );
}

// MARK: - session.rename

#[test]
fn rename_trims_and_a_blank_name_restores_the_automatic_one() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let automatic = fixture.store().session(id).unwrap().display_name();
    let response = fixture.session(
        SessionCmd::Rename,
        Some(&wire(id)),
        args(|a| a.name = Some("  custom  ".into())),
    );
    assert_eq!(answered_id(&response), Some(id));
    assert_eq!(fixture.store().session(id).unwrap().display_name(), "custom");
    fixture.session(
        SessionCmd::Rename,
        Some(&wire(id)),
        args(|a| a.name = Some("   ".into())),
    );
    assert_eq!(fixture.store().session(id).unwrap().display_name(), automatic);
}

#[test]
fn rename_requires_a_name_and_a_known_target() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        fixture.session(SessionCmd::Rename, Some("active"), Args::default()),
        failure("session.rename requires a name")
    );
    assert_eq!(
        fixture.session(SessionCmd::Rename, Some(UNKNOWN), args(|a| a.name = Some("x".into()))),
        failure("no such session: zzz")
    );
}

// MARK: - session.reveal

/// A core whose reveal handler records the directories it is handed.
fn recording(fixture: &mut Fixture) -> Rc<RefCell<Vec<PathBuf>>> {
    let revealed = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&revealed);
    fixture
        .core
        .set_reveal_handler(Box::new(move |path| sink.borrow_mut().push(path.to_owned())));
    revealed
}

#[test]
fn reveal_hands_the_focused_directory_to_the_frontend() {
    let directory = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(0);
    let revealed = recording(&mut fixture);
    let id = fixture.sessions[0];
    let path = directory.path().to_string_lossy().into_owned();
    fixture
        .store_mut()
        .unwrap()
        .session_mut(id)
        .unwrap()
        .set_current_cwd(&path);
    let response = fixture.session(SessionCmd::Reveal, Some(&wire(id)), Args::default());
    assert_eq!(answered_id(&response), Some(id));
    assert_eq!(*revealed.borrow(), [directory.path().to_owned()]);
}

#[test]
fn reveal_refuses_a_missing_directory_without_calling_the_frontend() {
    let mut fixture = Fixture::new(0);
    let revealed = recording(&mut fixture);
    let id = fixture.sessions[0];
    fixture
        .store_mut()
        .unwrap()
        .session_mut(id)
        .unwrap()
        .set_current_cwd("/agterm/no/such/dir");
    assert_eq!(
        fixture.session(SessionCmd::Reveal, None, Args::default()),
        failure("session cwd is not an existing directory")
    );
    assert!(revealed.borrow().is_empty());
    assert_eq!(
        fixture.session(SessionCmd::Reveal, Some(UNKNOWN), Args::default()),
        failure("no such session: zzz")
    );
}

#[test]
fn reveal_without_a_handler_still_answers_after_the_check() {
    let home = tempfile::tempdir().unwrap();
    let mut library = WindowLibrary::new(home.path().to_string_lossy());
    library.new_window(None);
    let mut core = core_over(library, AppSettings::default());
    // the seeded session starts at home; a `~` cwd expands to it too
    let response: Response = send(&mut core, Command::Session(SessionCmd::Reveal), None, Args::default());
    let id = answered_id(&response).unwrap();
    let created = send(
        &mut core,
        Command::Session(SessionCmd::New),
        None,
        args(|a| a.cwd = Some("~".into())),
    );
    let tilde: Uuid = answered_id(&created).unwrap();
    assert_ne!(id, tilde);
    let response = send(
        &mut core,
        Command::Session(SessionCmd::Reveal),
        Some(&wire(tilde)),
        Args::default(),
    );
    assert_eq!(answered_id(&response), Some(tilde));
}
