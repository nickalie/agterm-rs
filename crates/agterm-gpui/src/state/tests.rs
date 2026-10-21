//! The settle cases carried over from the GTK build (`agterm-gtk/src/state.rs`), plus the frontend work the GPUI
//! loop lands with the app context.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agterm_core::PaneRole;
use agterm_core::events::{EventCursor, ReadOptions};
use agterm_core::model::{NewSession, WindowLibrary};
use agterm_core::persist::{LibraryPersistence, SAVE_DEBOUNCE};
use agterm_core::{CoreConfig, CoreState};
use agterm_host::fakes::FakeSurface;
use agterm_host::{MainLoopClosed, MainTask};
use agterm_proto::command::WindowCmd;
use agterm_proto::{AppIdentity, Args, Command, EventKind, Request};

use super::*;

/// A loop that records what is posted, with its delay, for the test to run.
struct RecordingLoop<S> {
    posted: Mutex<Vec<(Option<Duration>, MainTask<S>)>>,
}

impl<S> RecordingLoop<S> {
    fn new() -> Self {
        Self {
            posted: Mutex::new(Vec::new()),
        }
    }

    fn take(&self) -> Vec<(Option<Duration>, MainTask<S>)> {
        std::mem::take(&mut *self.posted.lock().unwrap())
    }
}

impl<S> MainLoop<S> for RecordingLoop<S> {
    fn post(&self, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        self.posted.lock().unwrap().push((None, task));
        Ok(())
    }

    fn post_after(&self, delay: Duration, task: MainTask<S>) -> Result<(), MainLoopClosed> {
        self.posted.lock().unwrap().push((Some(delay), task));
        Ok(())
    }
}

fn core(library: WindowLibrary, persistence: Option<LibraryPersistence>) -> Core {
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            persistence,
            ..CoreState::default()
        },
    )
}

/// The loop the state posts to, and the strong handle its weak poster needs kept alive.
type Recorded = (Arc<RecordingLoop<AppState>>, Arc<dyn MainLoop<AppState>>);

/// Points `state` at a fresh recording loop; the caller keeps both handles.
fn record(state: &mut AppState) -> Recorded {
    let main_loop: Arc<RecordingLoop<AppState>> = Arc::new(RecordingLoop::new());
    let as_dyn: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
    state.poster = Arc::downgrade(&as_dyn);
    (main_loop, as_dyn)
}

#[test]
fn a_commit_arms_its_saves_grace_timers_and_tree_changed_with_their_delays() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut persistence = LibraryPersistence::new(dir.path());
    let library = persistence.bootstrap("/home/u");
    let window = library.active_window_id().unwrap();
    let mut core = core(library, Some(persistence));
    let store = core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let first = store.sessions().next().unwrap().id();
    store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    let structural = core.commit();
    assert_eq!(
        structural.tree_changed.len(),
        1,
        "a structural change invalidates the tree"
    );
    core.library_mut()
        .store_mut(window)
        .unwrap()
        .select_session(Some(first));
    let mut timers = core.commit();
    assert_eq!(timers.debounced.len(), 1, "a selection saves debounced");
    timers.tree_changed = structural.tree_changed;
    let timer = PendingCloseTimer {
        window,
        record: Uuid::new_v4(),
        grace: Duration::from_secs(3),
    };
    timers.pending_closes.push(timer);

    let main_loop = RecordingLoop::<Vec<String>>::new();
    let handlers = TimerHandlers {
        save: |seen: &mut Vec<String>, save| seen.push(format!("save {}", save.window)),
        finalize: |seen: &mut Vec<String>, timer| seen.push(format!("finalize {}", timer.record)),
        tree_changed: |seen: &mut Vec<String>, timer| seen.push(format!("tree {}", timer.window)),
    };
    arm_commit(&main_loop, &timers, &handlers);
    let mut seen = Vec::new();
    let delays: Vec<Option<Duration>> = main_loop
        .take()
        .into_iter()
        .map(|(delay, task)| {
            task(&mut seen);
            delay
        })
        .collect();
    assert_eq!(
        delays,
        [
            Some(SAVE_DEBOUNCE),
            Some(Duration::from_secs(3)),
            Some(TREE_CHANGED_DEBOUNCE)
        ]
    );
    assert_eq!(
        seen,
        [
            format!("save {window}"),
            format!("finalize {}", timer.record),
            format!("tree {window}")
        ]
    );
}

#[test]
fn settle_sequences_events_and_lands_tree_changed_when_its_timer_fires() {
    let (mut state, window, _) = state_with_terminal();
    let (main_loop, _held) = record(&mut state);
    state.settle();
    let _ = main_loop.take();
    let tail = state.core.events().ring().current_sequence();

    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    state.settle();
    let ring = state.core.events().ring();
    assert_eq!(ring.current_sequence(), tail + 2, "created and selected, at once");
    let posted = main_loop.take();
    assert_eq!(posted.len(), 1);
    for (delay, task) in posted {
        assert_eq!(delay, Some(TREE_CHANGED_DEBOUNCE));
        task(&mut state);
    }
    let options = ReadOptions {
        cursor: Some(EventCursor {
            run: state.core.events().ring().run(),
            after: tail + 2,
        }),
        ..ReadOptions::default()
    };
    let batch = state.core.events().read(&options).unwrap();
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.items[0].kind, EventKind::TreeChanged);
    assert_eq!(batch.items[0].window, Some(agterm_core::pane_env::uuid_string(window)));
}

#[test]
fn settle_writes_a_debounced_save_when_its_timer_fires() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut persistence = LibraryPersistence::new(dir.path());
    let library = persistence.bootstrap("/home/u");
    let window = library.active_window_id().unwrap();
    let path = persistence.window_path(window);
    let mut state = AppState::new(core(library, Some(persistence)));
    let (main_loop, _held) = record(&mut state);
    state.settle();
    let _ = main_loop.take();

    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let added = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    state.settle();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(
        saved.contains(&agterm_core::pane_env::uuid_string(added)),
        "a structural change saves now"
    );
    let _ = main_loop.take();

    let first = state
        .core
        .library()
        .store(window)
        .unwrap()
        .sessions()
        .next()
        .unwrap()
        .id();
    state
        .core
        .library_mut()
        .store_mut(window)
        .unwrap()
        .select_session(Some(first));
    state.settle();
    let posted = main_loop.take();
    assert_eq!(posted.len(), 1);
    let before = std::fs::read_to_string(&path).unwrap();
    for (delay, task) in posted {
        assert_eq!(delay, Some(SAVE_DEBOUNCE));
        task(&mut state);
    }
    let after = std::fs::read_to_string(&path).unwrap();
    assert_ne!(before, after, "the fired save wrote the new selection");
    assert!(after.contains(&agterm_core::pane_env::uuid_string(first)));
}

#[test]
fn nothing_saves_after_the_quit_save() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut persistence = LibraryPersistence::new(dir.path());
    let library = persistence.bootstrap("/home/u");
    let window = library.active_window_id().unwrap();
    let path = persistence.window_path(window);
    let mut state = AppState::new(core(library, Some(persistence)));
    state.save_on_quit();
    assert!(state.is_quitting());
    let saved = std::fs::read_to_string(&path).unwrap();
    let session = state
        .core
        .library()
        .store(window)
        .unwrap()
        .sessions()
        .next()
        .unwrap()
        .id();
    let token = "T".to_owned();
    state
        .core
        .library_mut()
        .store_mut(window)
        .unwrap()
        .session_mut(session)
        .unwrap()
        .surface
        .replace(Box::new(FakeSurface::new(token.clone())));
    state.pane_exited(session, &token);
    assert!(state.core.library().store(window).unwrap().session(session).is_some());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
}

/// A seeded window whose first session's primary terminal is a fake with pane token `T`.
fn state_with_terminal() -> (AppState, Uuid, Uuid) {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let session = store.sessions().next().unwrap().id();
    store
        .session_mut(session)
        .unwrap()
        .surface
        .replace(Box::new(FakeSurface::new("T")));
    (AppState::new(core(library, None)), window, session)
}

#[test]
fn a_command_waiting_on_its_terminal_answers_from_a_delayed_step() {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let session = store.sessions().next().unwrap().id();
    let surface = FakeSurface::new("T").with_realized(false);
    let probe = surface.probe();
    store.session_mut(session).unwrap().surface.replace(Box::new(surface));
    let mut state = AppState::new(core(library, None));
    let (main_loop, _held) = record(&mut state);

    let request = Request {
        cmd: Command::Session(SessionCmd::Type),
        target: None,
        args: Some(agterm_proto::Args {
            text: Some("late".to_owned()),
            ..agterm_proto::Args::default()
        }),
    };
    let reply = state.handle_request(&request);
    let posted = main_loop.take();
    let step = posted
        .into_iter()
        .find(|(delay, _)| *delay == Some(agterm_core::POLL_INTERVAL));
    let (_, step) = step.expect("a delayed step");
    probe.set_realized(true);
    step(&mut state);
    let reply = reply.settled();
    assert!(reply.response.ok, "{:?}", reply.response);
    assert_eq!(probe.typed(), ["late"]);
}

#[test]
fn a_waiting_command_whose_step_is_dropped_answers_shutting_down() {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let session = store.sessions().next().unwrap().id();
    let surface = FakeSurface::new("T").with_realized(false);
    store.session_mut(session).unwrap().surface.replace(Box::new(surface));
    let mut state = AppState::new(core(library, None));
    let (main_loop, _held) = record(&mut state);
    let request = Request {
        cmd: Command::Session(SessionCmd::Type),
        target: None,
        args: Some(agterm_proto::Args {
            text: Some("never".to_owned()),
            ..agterm_proto::Args::default()
        }),
    };
    let reply = state.handle_request(&request);
    // the loop closed: its queued and delayed tasks are dropped unrun
    drop(main_loop.take());
    let reply = reply.settled();
    assert!(!reply.response.ok);
}

#[test]
fn the_primary_shell_exiting_closes_a_session_without_a_split() {
    let (mut state, window, session) = state_with_terminal();
    state.pane_exited(session, "other");
    assert!(
        state.core.library().store(window).unwrap().session(session).is_some(),
        "not its terminal"
    );
    state.pane_exited(session, "T");
    assert!(state.core.library().store(window).unwrap().session(session).is_none());
    state.pane_exited(session, "T");
}

#[test]
fn a_terminal_report_lands_on_its_session_in_any_window() {
    let (mut state, window, session) = state_with_terminal();
    state.core.library_mut().new_window(None);
    assert_ne!(state.core.library().active_window_id(), Some(window));
    state.report_pane(session, "T", &PaneReport::Pwd("/tmp/next".to_owned()));
    state.report_pane(session, "T", &PaneReport::Title("vim".to_owned()));
    state.report_pane(Uuid::new_v4(), "T", &PaneReport::Title("lost".to_owned()));
    state.report_pane(session, "other", &PaneReport::Title("stale".to_owned()));
    let reported = state.core.library().store(window).unwrap().session(session).unwrap();
    assert_eq!(reported.effective_cwd(), "/tmp/next");
    assert_eq!(reported.osc_title.as_deref(), Some("vim"));
    assert_eq!(reported.display_name(), "next", "the title names no session by default");
}

#[test]
fn a_terminal_report_lands_on_a_session_a_soft_close_holds() {
    let (mut state, window, session) = state_with_terminal();
    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    assert!(store.soft_close_session(session));
    assert!(
        store.session(session).is_none(),
        "out of the tree, held by the pending close"
    );
    state.report_pane(session, "T", &PaneReport::Pwd("/tmp/held".to_owned()));
    state.report_pane(session, "T", &PaneReport::Title("htop".to_owned()));
    let store = state.core.library_mut().store_mut(window).unwrap();
    assert!(store.undo_pending_close(None));
    let restored = store.session(session).unwrap();
    assert_eq!(
        restored.effective_cwd(),
        "/tmp/held",
        "the grace-period cwd survives the undo"
    );
    assert_eq!(restored.osc_title.as_deref(), Some("htop"));
}

/// [`state_with_terminal`] with a shown split whose pane is a fake with token `S`, the split focused.
fn state_with_split() -> (AppState, Uuid, Uuid) {
    let (mut state, window, session) = state_with_terminal();
    let store = state.core.library_mut().store_mut(window).unwrap();
    store.set_split_visibility(session, true, None);
    store
        .session_mut(session)
        .unwrap()
        .split_surface
        .replace(Box::new(FakeSurface::new("S")));
    (state, window, session)
}

#[test]
fn the_split_shell_exiting_collapses_the_split_and_the_primary_promotes_it() {
    let (mut state, window, session) = state_with_split();
    state.pane_exited(session, "S");
    let found = state.core.library().store(window).unwrap().session(session).unwrap();
    assert!(!found.split.exists && !found.split_surface.is_present());

    let (mut state, window, session) = state_with_split();
    state.pane_exited(session, "T");
    let found = state.core.library().store(window).unwrap().session(session).unwrap();
    assert_eq!(found.surface.get().map(agterm_host::Surface::pane_token), Some("S"));
    assert!(!found.split.exists);
}

#[test]
fn a_split_pane_report_lands_on_the_split_fields() {
    let (mut state, window, session) = state_with_split();
    state.report_pane(session, "S", &PaneReport::Pwd("/srv/right".to_owned()));
    state.report_pane(session, "S", &PaneReport::Title("htop".to_owned()));
    let found = state.core.library().store(window).unwrap().session(session).unwrap();
    assert_eq!(found.split_cwd.as_deref(), Some("/srv/right"));
    assert_eq!(found.split_title.as_deref(), Some("htop"));
    assert_eq!(found.effective_cwd(), "/home/u", "the primary's stays");
}

#[test]
fn focusing_a_shown_split_pane_moves_the_split_focus_but_never_a_hidden_ones() {
    let (mut state, window, session) = state_with_split();
    let focused = |state: &AppState| {
        state
            .core
            .library()
            .store(window)
            .unwrap()
            .session(session)
            .unwrap()
            .split
            .focused
    };
    assert!(focused(&state));
    state.pane_focused(session, "T");
    assert!(!focused(&state));
    state.pane_focused(session, "S");
    assert!(focused(&state));
    let store = state.core.library_mut().store_mut(window).unwrap();
    store.set_split_visibility(session, false, None);
    state.pane_focused(session, "T");
    assert!(focused(&state), "a hidden split keeps its chosen pane");
}

#[test]
fn a_dragged_divider_stores_its_ratio_clamped() {
    let (mut state, window, session) = state_with_split();
    state.set_split_ratio(session, 0.3);
    let ratio = |state: &AppState| {
        state
            .core
            .library()
            .store(window)
            .unwrap()
            .session(session)
            .unwrap()
            .split
            .ratio
    };
    assert_eq!(ratio(&state), Some(0.3));
    state.set_split_ratio(session, 2.0);
    assert_eq!(ratio(&state), Some(agterm_core::model::pane::SPLIT_RATIO_MAX));
    state.set_split_ratio(Uuid::new_v4(), 0.5);
}

#[test]
fn closing_a_window_saves_it_closes_its_store_and_tears_its_terminals_down() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut persistence = LibraryPersistence::new(dir.path());
    let mut library = persistence.bootstrap("/home/u");
    let first = library.active_window_id().unwrap();
    let second = library.new_window(Some("second"));
    let path = persistence.window_path(second);
    let store = library.store_mut(second).unwrap();
    let session = store.sessions().next().unwrap().id();
    let terminal = FakeSurface::new("T");
    let probe = terminal.probe();
    store.session_mut(session).unwrap().surface.replace(Box::new(terminal));
    let mut state = AppState::new(core(library, Some(persistence)));
    // upstream's `windowShouldClose`: a window running sessions asks first, once
    state.close_requested(second);
    state.close_requested(second);
    assert!(state.core.library().is_open(second), "nothing closes before the answer");
    assert_eq!(probe.teardowns(), 0);
    assert_eq!(
        state.take_frontend_work().window_closes,
        [crate::state::WindowClose {
            window: second,
            name: "second".to_owned(),
            sessions: 1,
        }]
    );
    state.close_confirmed(second);
    assert!(!state.core.library().is_open(second));
    assert_eq!(state.core.library().windows().len(), 2, "its bundle stays");
    assert_eq!(state.core.library().active_window_id(), Some(first));
    assert_eq!(probe.teardowns(), 1, "its terminals go with it");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(
        saved.contains(&agterm_core::pane_env::uuid_string(session)),
        "written before it closed"
    );
}

/// Upstream's `clearFlags`: nothing while no session is flagged, else the frontend asks first and only the
/// confirmation unflags.
#[test]
fn clear_flagged_asks_first_and_only_the_confirmation_unflags() {
    let library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let mut state = AppState::new(core(library, None));
    state.clear_flags_requested(window);
    assert_eq!(
        state.take_frontend_work().clear_flags,
        None,
        "nothing flagged: nothing to ask"
    );
    let flagged = |state: &AppState| state.core.library().store(window).unwrap().flagged_sessions().len();
    let store = state.core.library_mut().store_mut(window).unwrap();
    let session = store.sessions().next().unwrap().id();
    store.set_flag(true, session);
    state.clear_flags_requested(window);
    assert_eq!(state.take_frontend_work().clear_flags, Some((window, 1)));
    assert_eq!(flagged(&state), 1, "still flagged until confirmed");
    state.clear_flags_confirmed(window);
    assert_eq!(flagged(&state), 0);
}

#[test]
fn an_activated_window_becomes_frontmost_unless_quitting() {
    let mut library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let second = library.new_window(None);
    let mut state = AppState::new(core(library, None));
    state.report_frontmost(first);
    assert_eq!(state.core.library().active_window_id(), Some(first));
    state.save_on_quit();
    state.report_frontmost(second);
    assert_eq!(state.core.library().active_window_id(), Some(first));
}

/// What a window built now hands its first activation.
fn built(state: &AppState) -> FirstFocus {
    FirstFocus::Built {
        frontmost_takes: state.core.frontmost_takes(),
    }
}

/// `window.<cmd>` with `args`, untargeted.
fn window_command(state: &mut AppState, cmd: WindowCmd, args: Args) {
    let response = state.core.dispatch(&Request {
        cmd: Command::Window(cmd),
        target: None,
        args: Some(args),
    });
    assert!(response.ok, "{:?}", response.error);
}

#[test]
fn a_new_windows_first_focus_leaves_frontmost_to_a_later_command() {
    let mut library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    // `window.new` made the second window frontmost
    let second = library.new_window(None);
    let mut state = AppState::new(core(library, None));
    let first_focus = Some(built(&state));
    // `window.select` took frontmost back before the window manager focused the newly mapped second window
    let select = crate::keymap_actions::window_request(WindowCmd::Select, first, None);
    assert!(state.core.dispatch(&select).ok);
    state.window_activated(second, first_focus);
    assert_eq!(state.key_window(), Some(second), "it holds the keyboard");
    assert_eq!(
        state.core.library().frontmost_window_id,
        Some(first),
        "the later command keeps frontmost"
    );
    // any later activation is the user's
    state.window_activated(second, None);
    assert_eq!(state.core.library().frontmost_window_id, Some(second));
}

#[test]
fn the_answer_to_a_raise_leaves_frontmost_to_a_later_command() {
    let mut library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let second = library.new_window(None);
    let mut state = AppState::new(core(library, None));
    // `window.select` raises the first window, then `window.go` takes the second before the window manager answered
    let select = crate::keymap_actions::window_request(WindowCmd::Select, first, None);
    assert!(state.core.dispatch(&select).ok);
    let raised = FirstFocus::Raised {
        frontmost_takes: state.core.frontmost_takes(),
    };
    let go = crate::keymap_actions::window_request(WindowCmd::Select, second, None);
    assert!(state.core.dispatch(&go).ok);
    state.window_activated(first, Some(raised));
    assert_eq!(state.key_window(), Some(first), "it holds the keyboard");
    assert_eq!(
        state.core.library().frontmost_window_id,
        Some(second),
        "the later command keeps frontmost"
    );
    // with no command since the raise, its answer counts
    let raised = FirstFocus::Raised {
        frontmost_takes: state.core.frontmost_takes(),
    };
    state.window_activated(second, Some(raised));
    assert_eq!(state.core.library().frontmost_window_id, Some(second));
    let select = crate::keymap_actions::window_request(WindowCmd::Select, first, None);
    assert!(state.core.dispatch(&select).ok);
    let raised = FirstFocus::Raised {
        frontmost_takes: state.core.frontmost_takes(),
    };
    state.window_activated(first, Some(raised));
    assert_eq!(state.core.library().frontmost_window_id, Some(first));
    // the user's own activation always counts
    state.window_activated(second, None);
    assert_eq!(state.core.library().frontmost_window_id, Some(second));
}

#[test]
fn a_new_windows_first_focus_leaves_frontmost_to_a_later_new_window() {
    let library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let mut state = AppState::new(core(library, None));
    let new_window = |state: &mut AppState| {
        window_command(state, WindowCmd::New, Args::default());
        let library = state.core.library();
        library.frontmost_window_id.unwrap()
    };
    let earlier = new_window(&mut state);
    let earlier_focus = Some(built(&state));
    let later = new_window(&mut state);
    assert!(![first, earlier].contains(&later));
    // the window manager focuses the earlier window after the later `window.new`
    state.window_activated(earlier, earlier_focus);
    assert_eq!(state.core.library().frontmost_window_id, Some(later));
}

#[test]
fn a_new_windows_first_focus_counts_without_a_command_since() {
    let mut library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let second = library.new_window(None);
    let mut state = AppState::new(core(library, None));
    let first_focus = Some(built(&state));
    // the user went to the first window, then into the second one the window manager left unfocused
    state.window_activated(first, None);
    state.window_activated(second, first_focus);
    assert_eq!(state.core.library().frontmost_window_id, Some(second));
}

#[test]
fn a_reopened_windows_first_focus_counts() {
    let mut library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let second = library.new_window(None);
    library.close_window(second);
    library.set_frontmost(Some(first));
    let mut state = AppState::new(core(library, None));
    // a notification's click reopens its closed window, its session gone: no command took frontmost
    assert!(state.core.reveal_notification(second, Uuid::new_v4(), PaneRole::Main));
    assert!(state.core.library().is_open(second));
    assert_eq!(state.core.library().frontmost_window_id, Some(first));
    state.window_activated(second, Some(built(&state)));
    assert_eq!(state.core.library().frontmost_window_id, Some(second));
}

#[test]
fn a_clicked_banner_raises_its_window_with_the_servers_token() {
    let (mut state, window, session) = state_with_terminal();
    let identity = agterm_core::notification_identity(window, session, PaneRole::Main);
    state.reveal_notification(&identity, Some("token".to_owned()));
    assert_eq!(
        state.take_frontend_work().activations,
        [Activation {
            token: Some("token".to_owned()),
            startup_id: None,
        }],
        "the server's token raises the window"
    );
    state.reveal_notification(&identity, None);
    assert!(
        state.take_frontend_work().activations.is_empty(),
        "without a token the window request raises it"
    );
    state.reveal_notification("not an identity", Some("token".to_owned()));
    assert!(state.take_frontend_work().activations.is_empty(), "nothing to reveal");
}

#[test]
fn a_restored_windows_first_focus_counts_over_the_saved_frontmost() {
    let mut library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let saved = library.new_window(None);
    let mut state = AppState::new(core(library, None));
    assert_eq!(state.core.library().frontmost_window_id, Some(saved));
    // the window manager kept the keyboard on the first window mapped
    state.window_activated(first, Some(built(&state)));
    assert_eq!(state.core.library().frontmost_window_id, Some(first));
}

#[test]
fn a_parked_new_windows_first_focus_leaves_frontmost_where_its_command_handed_it() {
    let library = WindowLibrary::seeded("/home/u");
    let first = library.active_window_id().unwrap();
    let mut state = AppState::new(core(library, None));
    let args = Args {
        minimized: Some(true),
        ..Args::default()
    };
    window_command(&mut state, WindowCmd::New, args);
    let library = state.core.library();
    let second = library.open_ids().into_iter().find(|id| *id != first).unwrap();
    assert_eq!(library.frontmost_window_id, Some(first), "handed off");
    state.window_activated(second, Some(FirstFocus::Parked));
    assert_eq!(state.core.library().frontmost_window_id, Some(first));
}

#[test]
fn sidebar_actions_select_sessions_and_fold_workspaces() {
    let (mut state, window, first) = state_with_terminal();
    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let second = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    assert_eq!(store.selected_session_id(), Some(second));
    state.sidebar_action(
        window,
        SidebarAction::SelectSessions {
            active: first,
            selection: vec![first],
        },
    );
    let store = state.core.library().store(window).unwrap();
    assert_eq!(store.selected_session_id(), Some(first));
    state.sidebar_toggle_workspace(window, workspace, false);
    let store = state.core.library().store(window).unwrap();
    assert!(!store.workspace(workspace).unwrap().is_expanded);
    state.sidebar_toggle_workspace(window, workspace, true);
    let store = state.core.library().store(window).unwrap();
    assert!(store.workspace(workspace).unwrap().is_expanded);
}

/// Runs the armed zero-delay fires of `main_loop` on `state`; how many there were.
fn fire_zero_delays(main_loop: &RecordingLoop<AppState>, state: &mut AppState) -> usize {
    let fires: Vec<_> = main_loop
        .take()
        .into_iter()
        .filter(|(delay, _)| *delay == Some(Duration::ZERO))
        .collect();
    let count = fires.len();
    for (_, task) in fires {
        task(state);
    }
    count
}

#[test]
fn a_sidebar_rename_holds_auto_follow_until_it_ends() {
    use agterm_core::auto_follow::FollowConfig;
    use agterm_core::model::{AgentIndicator, AgentStatus};

    let (mut state, window, first) = state_with_terminal();
    let (main_loop, _held) = record(&mut state);
    state.settle();
    let _ = main_loop.take();
    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let second = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    let blocked = AgentIndicator {
        status: AgentStatus::Blocked,
        ..AgentIndicator::default()
    };
    store.set_agent_indicator(blocked, first);
    let selected = |state: &AppState| state.core.library().store(window).unwrap().selected_session_id();

    state.sidebar_rename_began(window);
    state.core.set_auto_follow(Some(FollowConfig {
        timeout: Duration::ZERO,
        stay_on_active: false,
    }));
    state.settle();
    assert_eq!(
        fire_zero_delays(&main_loop, &mut state),
        1,
        "a follow is armed mid-rename"
    );
    assert_eq!(
        selected(&state),
        Some(second),
        "but does not fire while the field is open"
    );

    state.sidebar_rename_ended(
        window,
        Some(SidebarAction::RenameSession {
            session: second,
            name: "build".into(),
        }),
    );
    let store = state.core.library().store(window).unwrap();
    assert_eq!(store.session(second).unwrap().display_name(), "build");
    // the suppression was lifted: the next armed follow jumps to the blocked session
    state.sidebar_action(
        window,
        SidebarAction::SelectSessions {
            active: second,
            selection: vec![second],
        },
    );
    assert_eq!(fire_zero_delays(&main_loop, &mut state), 1);
    assert_eq!(selected(&state), Some(first));
}

#[test]
fn a_sidebar_click_is_activity_and_an_armed_fire_follows_to_the_blocked_session() {
    use agterm_core::auto_follow::FollowConfig;
    use agterm_core::model::{AgentIndicator, AgentStatus};

    let (mut state, window, first) = state_with_terminal();
    state.set_key_window(Some(window));
    let (main_loop, _held) = record(&mut state);
    state.settle();
    let _ = main_loop.take();
    assert_eq!(state.core.tree(None).unwrap().idle_ms, None);
    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let second = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    let blocked = AgentIndicator {
        status: AgentStatus::Blocked,
        ..AgentIndicator::default()
    };
    store.set_agent_indicator(blocked, first);
    state.core.set_auto_follow(Some(FollowConfig {
        timeout: Duration::ZERO,
        stay_on_active: false,
    }));
    state.sidebar_action(
        window,
        SidebarAction::SelectSessions {
            active: second,
            selection: vec![second],
        },
    );
    assert!(state.core.tree(None).unwrap().idle_ms.is_some());
    assert_eq!(fire_zero_delays(&main_loop, &mut state), 1);
    let store = state.core.library().store(window).unwrap();
    assert_eq!(store.selected_session_id(), Some(first));
}

#[test]
fn quit_and_activation_requests_wait_for_the_frontend_in_order() {
    let (mut state, _, _) = state_with_terminal();
    assert_eq!(state.take_frontend_work(), FrontendWork::default());
    let first = Activation {
        token: Some("a".to_owned()),
        startup_id: None,
    };
    let second = Activation {
        token: None,
        startup_id: Some("b_TIME7".to_owned()),
    };
    state.activation_requested(first.clone());
    state.activation_requested(second.clone());
    state.request_quit();
    assert_eq!(
        state.take_frontend_work(),
        FrontendWork {
            activations: vec![first, second],
            quit: true,
            ..FrontendWork::default()
        }
    );
    assert_eq!(state.take_frontend_work(), FrontendWork::default(), "taken once");
}

#[test]
fn no_window_is_raised_once_the_quit_save_ran() {
    let (mut state, _, _) = state_with_terminal();
    state.save_on_quit();
    state.activation_requested(Activation::default());
    state.request_quit();
    assert_eq!(
        state.take_frontend_work(),
        FrontendWork {
            activations: Vec::new(),
            quit: true,
            ..FrontendWork::default()
        }
    );
}

#[test]
fn attaching_settles_and_arms_on_the_loop() {
    let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
    let mut persistence = LibraryPersistence::new(dir.path());
    let library = persistence.bootstrap("/home/u");
    let window = library.active_window_id().unwrap();
    let mut state = AppState::new(core(library, Some(persistence)));
    let main_loop: Arc<RecordingLoop<AppState>> = Arc::new(RecordingLoop::new());
    let as_dyn: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
    state.attach(Arc::downgrade(&as_dyn));
    assert!(state.poster().upgrade().is_some());
    let store = state.core.library_mut().store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    state.settle();
    assert!(
        main_loop
            .take()
            .iter()
            .any(|(delay, _)| *delay == Some(TREE_CHANGED_DEBOUNCE)),
        "the attached loop gets the commit's timers"
    );
    drop((as_dyn, main_loop));
    assert!(state.poster().upgrade().is_none(), "held weakly");
}

#[test]
fn a_notify_lands_its_banner_and_its_sound_with_the_frontend() {
    let (mut state, window, session) = state_with_terminal();
    state
        .core
        .change_settings(agterm_core::settings_model::SettingsChange::Notifications(
            agterm_core::settings_model::NotificationsChange::Sound(Some("Glass".to_owned())),
        ));
    let response = state.core.dispatch(&Request {
        cmd: Command::Notify,
        target: Some(agterm_core::pane_env::uuid_string(session)),
        args: Some(Args {
            title: Some("t".to_owned()),
            body: Some("done".to_owned()),
            ..Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    state.settle();
    let work = state.take_frontend_work();
    assert_eq!(work.banners.len(), 1, "{:?}", work.banners);
    assert!(matches!(
        &work.banners[0],
        agterm_core::NotificationEffect::Post(post) if post.window == window && post.body == "done"
    ));
    assert_eq!(work.sounds, ["Glass"], "a posted batch sounds the notification sound");
    state.settle();
    assert_eq!(state.take_frontend_work(), FrontendWork::default(), "landed once");
}
