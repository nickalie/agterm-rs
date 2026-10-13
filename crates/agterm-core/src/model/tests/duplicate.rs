//! Duplicating a session, ported from upstream's `AppStoreDuplicateTests.swift` and the duplicate case of
//! `AppStoreEventTests.swift`. The contract: same workspace, right after the source, seeded with the source's live
//! focused-pane cwd, and nothing else carried over.

use agterm_host::fakes::FakeSurface;
use agterm_proto::events::EventKind;
use uuid::Uuid;

use super::support::{add, ids};
use crate::model::{AppStore, NewSession, Session, SessionSeed};

const HOME: &str = "/home/tester";

fn session(store: &AppStore, id: Uuid) -> &Session {
    store.session(id).expect("the session exists")
}

#[test]
fn a_duplicate_lands_right_after_the_source_and_is_selected() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let first = add(&mut store, ws, "/a");
    let last = add(&mut store, ws, "/c");

    let dupe = store.duplicate_session(first, HOME).unwrap();

    assert_eq!(ids(&store, ws), [first, dupe, last]);
    assert_eq!(session(&store, dupe).initial_cwd(), "/a");
    assert_eq!(store.selected_session_id(), Some(dupe));
}

#[test]
fn a_duplicate_starts_in_the_focused_split_panes_cwd() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let source = add(&mut store, ws, "/primary");
    let live = store.session_mut(source).unwrap();
    live.set_current_cwd("/primary");
    live.split.shown = true;
    live.split.exists = true;
    // a focused split always has a live split surface
    live.split_surface.replace(Box::new(FakeSurface::new("right")));
    live.split_cwd = Some("/split-pane".to_owned());
    live.split.focused = true;

    let dupe = store.duplicate_session(source, HOME).unwrap();

    assert_eq!(session(&store, dupe).initial_cwd(), "/split-pane");
}

#[test]
fn a_remote_sessions_duplicate_is_a_local_shell_under_the_local_directory_rule() {
    let root = tempfile::tempdir().unwrap();
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let spec = NewSession {
        remote_host: Some("user@box".to_owned()),
        ..NewSession::at_cwd(HOME)
    };
    let source = store.add_session(ws, spec).unwrap();

    let only_remote = root.path().join("only-on-the-remote");
    store
        .session_mut(source)
        .unwrap()
        .set_current_cwd(only_remote.to_str().unwrap());
    let fallback = store.duplicate_session(source, HOME).unwrap();
    assert_eq!(session(&store, fallback).initial_cwd(), HOME);
    assert_eq!(session(&store, fallback).remote_host(), None);

    let here = root.path().to_str().unwrap();
    store.session_mut(source).unwrap().set_current_cwd(here);
    let twin = store.duplicate_session(source, HOME).unwrap();
    assert_eq!(session(&store, twin).initial_cwd(), here);
}

#[test]
fn a_local_session_keeps_its_path_even_when_it_is_not_a_directory_here() {
    let local = Session::new(SessionSeed {
        initial_cwd: "/a".to_owned(),
        ..SessionSeed::default()
    });
    assert_eq!(
        local.local_working_directory("/does/not/exist", HOME),
        "/does/not/exist"
    );
}

#[test]
fn a_duplicate_follows_the_live_cwd() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let source = add(&mut store, ws, "/start");
    // an OSC 7 report after a `cd`
    store.session_mut(source).unwrap().set_current_cwd("/moved");

    let dupe = store.duplicate_session(source, HOME).unwrap();

    assert_eq!(session(&store, dupe).initial_cwd(), "/moved");
}

#[test]
fn a_duplicate_copies_only_the_directory() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let spec = NewSession {
        command: Some("ssh host".to_owned()),
        name: Some("prod".to_owned()),
        ..NewSession::at_cwd("/a")
    };
    let source = store.add_session(ws, spec).unwrap();
    let live = store.session_mut(source).unwrap();
    live.flagged = true;
    live.split.shown = true;
    live.split.exists = true;
    live.split_pane_identity = Some(Uuid::new_v4());
    live.font_size = Some(18.0);
    live.restore.command = Some("claude --resume abc".to_owned());
    live.split_restore.command = Some("tail -f /var/log/x".to_owned());
    live.restore.pending_command = Some("claude --resume abc".to_owned());
    live.split_restore.pending_command = Some("tail -f /var/log/x".to_owned());

    let dupe = store.duplicate_session(source, HOME).unwrap();

    let (dupe, source) = (session(&store, dupe), session(&store, source));
    assert_eq!(dupe.initial_cwd(), "/a");
    assert_eq!(dupe.custom_name, None);
    assert_eq!(dupe.launch.command, None);
    assert!(!dupe.flagged);
    assert!(!dupe.split.shown && !dupe.split.exists);
    assert_eq!(dupe.font_size, None);
    assert_ne!(dupe.id(), source.id());
    assert_ne!(dupe.pane_identity, source.pane_identity);
    assert_eq!(dupe.split_pane_identity, None);
    assert_eq!(
        (dupe.restore.command.as_ref(), dupe.split_restore.command.as_ref()),
        (None, None)
    );
    assert_eq!(
        (
            dupe.restore.pending_command.as_ref(),
            dupe.split_restore.pending_command.as_ref()
        ),
        (None, None)
    );
    assert_eq!(source.restore.command.as_deref(), Some("claude --resume abc"));
}

#[test]
fn duplicating_an_unknown_session_does_nothing() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    add(&mut store, ws, "/a");
    assert_eq!(store.duplicate_session(Uuid::new_v4(), HOME), None);
    assert_eq!(ids(&store, ws).len(), 1);
}

#[test]
fn a_duplicate_emits_one_created_edge() {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let source = add(&mut store, ws, "/a");
    store.take_outbox();

    let dupe = store.duplicate_session(source, HOME).unwrap();

    let created: Vec<(Option<Uuid>, Option<Uuid>)> = store
        .take_outbox()
        .events
        .into_iter()
        .filter(|event| event.kind == EventKind::SessionCreated)
        .map(|event| (event.session, event.workspace))
        .collect();
    assert_eq!(created, [(Some(dupe), Some(ws))]);
}
