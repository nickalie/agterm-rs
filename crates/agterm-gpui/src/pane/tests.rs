//! What a pane runs (ported from the GTK build's `deck.rs` spec tests) and what its terminal's events report.

use std::path::PathBuf;

use agterm_core::model::{NewSession, WindowLibrary};
use agterm_term::{ExitStatus, PaneEvent};

use super::*;

fn context() -> PaneContext {
    PaneContext {
        socket: "/tmp/a.sock".to_owned(),
        program_version: "1.2.3".to_owned(),
    }
}

/// A window of one workspace holding three sessions, the first selected.
pub(crate) fn three_sessions() -> (WindowLibrary, Uuid, Vec<Uuid>) {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let first = store.sessions().next().unwrap().id();
    let second = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    let third = store
        .add_session(
            workspace,
            NewSession {
                command: Some("htop".to_owned()),
                wait: true,
                select: false,
                ..NewSession::at_cwd("/var")
            },
        )
        .unwrap();
    store.select_session(Some(first));
    (library, window, vec![first, second, third])
}

#[test]
fn the_page_env_names_the_session_its_window_workspace_and_pane() {
    let (library, window, ids) = three_sessions();
    let store = library.store(window).unwrap();
    let workspace = store.workspaces()[0].id();
    for id in ids {
        let spec = PaneSpec::primary(window, store, id).unwrap();
        let session = store.session(id).unwrap();
        let env = spec.env(&context());
        assert_eq!(env["AGTERM_SESSION_ID"], pane_env::uuid_string(id));
        assert_eq!(env["AGTERM_WINDOW_ID"], pane_env::uuid_string(window));
        assert_eq!(env["AGTERM_WORKSPACE_ID"], pane_env::uuid_string(workspace));
        assert_eq!(env["AGTERM_PANE"], "left");
        assert_eq!(env["AGTERM_PANE_ID"], pane_env::uuid_string(session.pane_identity));
        assert_eq!(spec.token, env["AGTERM_PANE_ID"]);
        assert_eq!(env["AGTERM_SOCKET"], "/tmp/a.sock");
        assert_eq!(env["TERM_PROGRAM"], "agterm");
        assert_eq!(env["TERM_PROGRAM_VERSION"], "1.2.3");
    }
}

#[test]
fn a_session_pane_launches_its_own_command_in_its_cwd_once_seeded() {
    let (library, window, ids) = three_sessions();
    let store = library.store(window).unwrap();
    for id in ids {
        let spec = PaneSpec::primary(window, store, id).unwrap();
        let session = store.session(id).unwrap();
        let launch = spec.launch(&context());
        assert_eq!(launch.working_directory, Some(PathBuf::from(session.effective_cwd())));
        assert_eq!(launch.command, session.launch.command);
        assert_eq!(launch.wait_after_command, session.launch.wait);
        assert!(
            launch
                .env
                .iter()
                .any(|(key, value)| key == "AGTERM_PANE_ID" && *value == spec.token)
        );
        assert!(spec.gated(), "a session pane waits for its seed");
    }
}

#[test]
fn a_session_outside_the_tree_has_no_page_spec() {
    let (library, window, _) = three_sessions();
    assert_eq!(
        PaneSpec::primary(window, library.store(window).unwrap(), Uuid::new_v4()),
        None
    );
}

#[test]
fn the_split_pane_runs_in_its_own_cwd_with_its_identity_and_command() {
    let (mut library, window, ids) = three_sessions();
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    assert_eq!(
        PaneSpec::split(window, workspace, store.session(ids[0]).unwrap()),
        None,
        "no split yet"
    );
    store.set_split_visibility(ids[0], true, None);
    let session = store.session_mut(ids[0]).unwrap();
    session.split_cwd = Some("/srv/right".to_owned());
    session.split_launch.command = Some("htop".to_owned());
    session.split_launch.wait = true;
    let session = store.session(ids[0]).unwrap();
    let spec = PaneSpec::split(window, workspace, session).unwrap();
    let env = spec.env(&context());
    assert_eq!(env["AGTERM_PANE"], "right");
    let identity = pane_env::uuid_string(session.split_pane_identity.unwrap());
    assert_eq!(env["AGTERM_PANE_ID"], identity);
    assert_eq!(spec.token, identity);
    let launch = spec.launch(&context());
    assert_eq!(launch.working_directory, Some(PathBuf::from("/srv/right")));
    assert_eq!(launch.command.as_deref(), Some("htop"));
    assert!(launch.wait_after_command);
    assert_eq!(
        PaneSpec::of_token(window, store, ids[0], &identity),
        None,
        "no terminal carries the token yet"
    );
    store
        .session_mut(ids[0])
        .unwrap()
        .split_surface
        .replace(Box::new(agterm_host::fakes::FakeSurface::new(identity.clone())));
    assert_eq!(
        PaneSpec::of_token(window, store, ids[0], &identity),
        Some(spec),
        "a replacement inherits the pane's own spec"
    );
}

#[test]
fn each_scratch_terminal_gets_a_fresh_token_and_runs_its_command_once() {
    let (mut library, window, ids) = three_sessions();
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let session = store.session_mut(ids[1]).unwrap();
    session.scratch_command = Some("lazygit".to_owned());
    let first = PaneSpec::scratch(window, workspace, session);
    assert_eq!(first.command.as_deref(), Some("lazygit"));
    assert_eq!(session.scratch_command, None, "taken: it runs once");
    let env = first.env(&context());
    assert_eq!(env["AGTERM_PANE"], "scratch");
    assert_eq!(env["AGTERM_PANE_ID"], first.token);
    assert_eq!(first.cwd, "/srv");
    assert!(!first.wait && !first.gated(), "the scratch starts from its spec");
    let second = PaneSpec::scratch(window, workspace, session);
    assert_eq!(second.command, None, "a respawn is a login shell");
    assert_ne!(second.token, first.token);
    assert_eq!(
        PaneSpec::of_token(window, store, ids[1], &first.token),
        None,
        "no replacement spec for a scratch"
    );
}

#[test]
fn a_seed_runs_over_the_panes_own_environment() {
    let (library, window, ids) = three_sessions();
    let spec = PaneSpec::primary(window, library.store(window).unwrap(), ids[0]).unwrap();
    let seed = LaunchSeed {
        command: Some("agterm-session attach x".to_owned()),
        initial_input: Some("ls\n".to_owned()),
        wait_after_command: false,
        env: vec![
            ("AGTERM_LIVE_DIR".to_owned(), "/tmp/z".to_owned()),
            ("AGTERM_PANE".to_owned(), "over".to_owned()),
        ],
        live: true,
    };
    let launch = spec.seeded(&seed, &context());
    assert_eq!(launch.command.as_deref(), Some("agterm-session attach x"));
    assert_eq!(launch.initial_input.as_deref(), Some("ls\n"));
    let value = |key: &str| {
        launch
            .env
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(value("AGTERM_LIVE_DIR"), Some("/tmp/z"));
    assert_eq!(value("AGTERM_PANE"), Some("over"), "the seed's variables win");
    assert_eq!(value("AGTERM_SESSION_ID").map(str::len), Some(36));
}

fn exited(held: bool) -> TerminalEvent {
    TerminalEvent::Pane(PaneEvent::Exited(ExitStatus {
        code: 0,
        signal: None,
        held,
    }))
}

#[test]
fn a_pane_events_report_what_the_gtk_build_routed() {
    let shell = Ran::default();
    let waited = Ran {
        command: true,
        wait: true,
    };
    let title = TerminalEvent::Pane(PaneEvent::Title("vim".to_owned()));
    assert_eq!(
        report(&title, shell, "box", 12.0, false),
        [Report::Pane(PaneReport::Title("vim".to_owned()))]
    );
    let pwd = TerminalEvent::Pane(PaneEvent::Pwd("file://box/srv/a%20b".to_owned()));
    assert_eq!(
        report(&pwd, shell, "box", 12.0, false),
        [Report::Pane(PaneReport::Pwd("/srv/a b".to_owned()))]
    );
    let foreign = TerminalEvent::Pane(PaneEvent::Pwd("file://elsewhere/srv".to_owned()));
    assert!(
        report(&foreign, shell, "box", 12.0, false).is_empty(),
        "a foreign host is ignored"
    );
    assert_eq!(report(&exited(false), shell, "box", 12.0, false), [Report::Exited]);
    assert!(
        report(&exited(true), shell, "box", 12.0, false).is_empty(),
        "a held login shell waits for a key"
    );
    assert_eq!(
        report(&exited(true), waited, "box", 12.0, false),
        [Report::Held],
        "a held command is reported held"
    );
    assert_eq!(
        report(&TerminalEvent::FontSize, shell, "box", 13.5, false),
        [Report::FontSize(13.5)]
    );
    assert_eq!(
        report(&TerminalEvent::StartSearch(String::new()), shell, "box", 12.0, false),
        [Report::Search(SearchReport::Started(None))]
    );
    assert_eq!(
        report(&TerminalEvent::StartSearch("x".to_owned()), shell, "box", 12.0, false),
        [Report::Search(SearchReport::Started(Some("x".to_owned())))]
    );
    assert_eq!(
        report(
            &TerminalEvent::Pane(PaneEvent::SearchTotal(Some(3))),
            shell,
            "box",
            12.0,
            false
        ),
        [Report::Search(SearchReport::Total(Some(3)))]
    );
    assert_eq!(
        report(
            &TerminalEvent::OpenUrl("https://x.dev".to_owned()),
            shell,
            "box",
            12.0,
            false
        ),
        [Report::OpenUrl("https://x.dev".to_owned())]
    );
    assert!(report(&TerminalEvent::Pane(PaneEvent::Bell), shell, "box", 12.0, false).is_empty());
}

#[test]
fn a_key_on_a_lead_covered_pane_is_not_a_keystroke() {
    let shell = Ran::default();
    let key = TerminalEvent::Keystroke(StatusKeystroke::Interrupt);
    assert_eq!(
        report(&key, shell, "box", 12.0, false),
        [Report::Keystroke(StatusKeystroke::Interrupt)]
    );
    assert!(
        report(&key, shell, "box", 12.0, true).is_empty(),
        "the takeover key is neither user activity nor a status clear"
    );
    // a cover hides only the takeover key: the pane's other events still report
    assert_eq!(report(&exited(false), shell, "box", 12.0, true), [Report::Exited]);
}
