//! The main loop's state: the core and the window it shows, and the tasks that change them.
//!
//! RULE: every mutation runs as a main-loop task and ends with [`AppState::settle`], which lands the model's side
//! effects (the immediate saves, the debounced saves and the soft closes' grace timers armed on the loop) and brings
//! the deck, the sidebar and the window title in line with the model. GTK and libghostty handlers never touch the
//! state in place: they [`post`] a task, since libghostty may call back while the state is borrowed and a widget
//! must not be rebuilt inside its own handler.

use std::sync::Weak;

use agterm_core::Core;
use agterm_core::model::PendingCloseTimer;
use agterm_core::persist::{Commit, DebouncedSave, arm_debounced_saves};
use agterm_host::MainLoop;
use uuid::Uuid;

use crate::main_loop::GlibMainLoop;
use crate::sidebar::{RowKind, SidebarRow};
use crate::window::WindowUi;

/// Where handlers post tasks: the main loop, held weakly so a handler outliving it posts nothing.
pub type Poster = Weak<dyn MainLoop<AppState>>;

/// A poster that reaches no loop, until the real one is attached.
#[must_use]
pub fn detached_poster() -> Poster {
    Weak::<GlibMainLoop<AppState>>::new()
}

/// Queues `task` on the main loop; a closed or gone loop drops it.
pub fn post(poster: &Poster, task: impl FnOnce(&mut AppState) + Send + 'static) {
    let Some(main_loop) = poster.upgrade() else {
        return;
    };
    if main_loop.post(Box::new(task)).is_err() {
        tracing::debug!("main-loop task dropped: the loop is closed");
    }
}

/// Arms what a commit leaves to the main loop: each debounced save fires after `SAVE_DEBOUNCE` through `fire`, each
/// soft close's grace timer after its grace through `finalize`. A closed loop drops them: the quit path writes
/// everything and makes every pending close final anyway.
pub fn arm_commit<S: 'static>(
    main_loop: &dyn MainLoop<S>,
    commit: &Commit,
    fire: fn(&mut S, DebouncedSave),
    finalize: fn(&mut S, PendingCloseTimer),
) {
    arm_debounced_saves(main_loop, &commit.debounced, fire);
    for timer in commit.pending_closes.iter().copied() {
        let _ = main_loop.post_after(timer.grace, Box::new(move |state: &mut S| finalize(state, timer)));
    }
}

/// A terminal report bound for the session that shows the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneReport {
    /// An OSC 7 working directory.
    Pwd(String),
    /// A title a program set (or the user's static one).
    Title(String),
}

/// What the main loop owns.
pub struct AppState {
    pub core: Core,
    poster: Poster,
    /// The window; `None` until the first activation built it (and in tests).
    ui: Option<WindowUi>,
    /// The quit save ran: nothing is committed, saved or re-rendered after it.
    quitting: bool,
}

impl AppState {
    #[must_use]
    pub fn new(core: Core) -> Self {
        Self {
            core,
            poster: detached_poster(),
            ui: None,
            quitting: false,
        }
    }

    /// Connects the state to its main loop and its window, and renders the model into the window.
    pub fn attach(&mut self, poster: Poster, ui: WindowUi) {
        self.poster = poster;
        self.ui = Some(ui);
        self.settle();
        self.window_state_changed();
    }

    /// Shows the window, the shown terminal focused.
    pub fn present(&self) {
        if let Some(ui) = self.ui.as_ref() {
            ui.present();
        }
    }

    /// The end of every mutation (module docs): commit, arm the loop's timers, re-render.
    pub fn settle(&mut self) {
        if self.quitting {
            return;
        }
        // the events go to the ring with Task 12
        let commit = self.core.commit();
        if let Some(main_loop) = self.poster.upgrade() {
            arm_commit(&*main_loop, &commit, fire_debounced, finalize_pending_close);
        }
        if let Some(ui) = self.ui.as_mut() {
            ui.sync(&mut self.core, &self.poster);
        }
    }

    /// The quit path's save (upstream's `applicationWillTerminate`), once, before any terminal is torn down: every
    /// pending close made final and every open window and the index written. Nothing is saved after it, so the
    /// shells ending as their terminals go do not close their sessions on disk.
    pub fn save_on_quit(&mut self) {
        if self.quitting {
            return;
        }
        if !self.core.save_all() {
            tracing::error!("the quit save did not write everything");
        }
        self.quitting = true;
    }

    /// A primary pane's shell ended: its session closes (upstream's `handlePaneExit` → `closePrimaryPane` →
    /// `closeSession` without a split), unless the session's terminal is another one by now.
    pub fn close_exited_session(&mut self, session: Uuid, token: &str) {
        if self.quitting {
            return;
        }
        let Some(store) = self.core.library_mut().store_for_session_mut(session) else {
            return;
        };
        let current = store
            .session(session)
            .and_then(|session| session.surface.get())
            .is_some_and(|surface| surface.pane_token() == token);
        if current {
            store.close_session(session);
            self.settle();
        }
    }

    /// Lands a primary pane's report on its session, wherever that session lives now (upstream's
    /// `applyPwd`/`applyTitle`); a session gone since is skipped. Nothing is saved, as upstream.
    pub fn apply_report(&mut self, session: Uuid, report: &PaneReport) {
        let Some(session) = self
            .core
            .library_mut()
            .store_for_session_mut(session)
            .and_then(|store| store.session_mut(session))
        else {
            return;
        };
        match report {
            PaneReport::Pwd(pwd) => session.report_pwd(false, pwd),
            PaneReport::Title(title) => session.report_title(false, title),
        }
        self.settle();
    }

    /// A sidebar row was clicked: a session row selects its session, a workspace row opens or folds the workspace
    /// and persists that (upstream's whole-row toggle, a genuine user expand).
    pub fn sidebar_clicked(&mut self, row: &SidebarRow) {
        let library = self.core.library_mut();
        match row.kind {
            RowKind::Session { .. } => {
                if let Some(store) = library.store_for_session_mut(row.id) {
                    store.select_session(Some(row.id));
                }
            }
            RowKind::Workspace { expanded } => {
                let window = library.open_ids().into_iter().find(|id| {
                    library
                        .store(*id)
                        .is_some_and(|store| store.workspace(row.id).is_some())
                });
                if let Some(store) = window.and_then(|id| library.store_mut(id)) {
                    store.set_workspace_expanded(row.id, !expanded);
                }
                if let Some(ui) = self.ui.as_mut() {
                    ui.set_workspace_expanded(row.id, !expanded);
                }
            }
        }
        self.settle();
    }

    /// The user dragged the sidebar divider: the shown window's sidebar takes the new width.
    pub fn sidebar_resized(&mut self) {
        let Some((window, width)) = self.ui.as_ref().and_then(WindowUi::dragged_sidebar_width) else {
            return;
        };
        if let Some(store) = self.core.library_mut().store_mut(window) {
            store.set_sidebar_width(width);
        }
        self.settle();
    }

    /// Reports the window's full-screen and maximized state for `window.list`.
    pub fn window_state_changed(&mut self) {
        if let Some((window, state)) = self.ui.as_ref().and_then(WindowUi::window_state) {
            self.core.set_window_state(window, Some(state));
        }
    }

    /// The user closed the window: the quit save runs while every terminal still lives, the window leaves
    /// `window.list`, then the window closes for real (and with it the app).
    pub fn close_requested(&mut self) {
        self.save_on_quit();
        let Some(ui) = self.ui.as_ref() else {
            return;
        };
        if let Some(window) = ui.window_id() {
            self.core.set_window_state(window, None);
        }
        ui.close();
    }
}

fn fire_debounced(state: &mut AppState, save: DebouncedSave) {
    if !state.quitting {
        state.core.fire_debounced(save);
    }
}

fn finalize_pending_close(state: &mut AppState, timer: PendingCloseTimer) {
    if state.quitting {
        return;
    }
    state.core.finalize_pending_close(timer);
    state.settle();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use agterm_core::model::{NewSession, WindowLibrary};
    use agterm_core::persist::{LibraryPersistence, SAVE_DEBOUNCE};
    use agterm_core::{CoreConfig, CoreState};
    use agterm_host::fakes::FakeSurface;
    use agterm_host::{MainLoopClosed, MainTask};
    use agterm_proto::AppIdentity;

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

    #[test]
    fn a_commit_arms_its_saves_and_grace_timers_with_their_delays() {
        let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
        let mut persistence = LibraryPersistence::new(dir.path());
        let mut library = persistence.bootstrap("/home/u");
        let window = library.active_window_id().unwrap();
        let store = library.store_mut(window).unwrap();
        let workspace = store.workspaces()[0].id();
        let first = store.sessions().next().unwrap().id();
        store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
        let _ = persistence.commit(&mut library);
        library.store_mut(window).unwrap().select_session(Some(first));
        let mut commit = persistence.commit(&mut library);
        assert_eq!(commit.debounced.len(), 1, "a selection saves debounced");
        let timer = PendingCloseTimer {
            window,
            record: Uuid::new_v4(),
            grace: Duration::from_secs(3),
        };
        commit.pending_closes.push(timer);

        let main_loop = RecordingLoop::<Vec<String>>::new();
        arm_commit(
            &main_loop,
            &commit,
            |seen, save| seen.push(format!("save {}", save.window)),
            |seen, timer| seen.push(format!("finalize {}", timer.record)),
        );
        let mut seen = Vec::new();
        let delays: Vec<Option<Duration>> = main_loop
            .take()
            .into_iter()
            .map(|(delay, task)| {
                task(&mut seen);
                delay
            })
            .collect();
        assert_eq!(delays, [Some(SAVE_DEBOUNCE), Some(Duration::from_secs(3))]);
        assert_eq!(seen, [format!("save {window}"), format!("finalize {}", timer.record)]);
    }

    #[test]
    fn settle_writes_a_debounced_save_when_its_timer_fires() {
        let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp").unwrap();
        let mut persistence = LibraryPersistence::new(dir.path());
        let library = persistence.bootstrap("/home/u");
        let window = library.active_window_id().unwrap();
        let path = persistence.window_path(window);
        let mut state = AppState::new(core(library, Some(persistence)));
        let main_loop: Arc<RecordingLoop<AppState>> = Arc::new(RecordingLoop::new());
        let as_dyn: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
        state.poster = Arc::downgrade(&as_dyn);
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
        state.close_exited_session(session, &token);
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
    fn the_primary_shell_exiting_closes_its_session() {
        let (mut state, window, session) = state_with_terminal();
        state.close_exited_session(session, "other");
        assert!(
            state.core.library().store(window).unwrap().session(session).is_some(),
            "not its terminal"
        );
        state.close_exited_session(session, "T");
        assert!(state.core.library().store(window).unwrap().session(session).is_none());
        state.close_exited_session(session, "T");
    }

    #[test]
    fn a_terminal_report_lands_on_its_session_in_any_window() {
        let (mut state, window, session) = state_with_terminal();
        state.core.library_mut().new_window(None);
        assert_ne!(state.core.library().active_window_id(), Some(window));
        state.apply_report(session, &PaneReport::Pwd("/tmp/next".to_owned()));
        state.apply_report(session, &PaneReport::Title("vim".to_owned()));
        state.apply_report(Uuid::new_v4(), &PaneReport::Title("lost".to_owned()));
        let reported = state.core.library().store(window).unwrap().session(session).unwrap();
        assert_eq!(reported.effective_cwd(), "/tmp/next");
        assert_eq!(reported.osc_title.as_deref(), Some("vim"));
        assert_eq!(reported.display_name(), "vim");
    }

    #[test]
    fn sidebar_clicks_select_sessions_and_fold_workspaces() {
        let (mut state, window, first) = state_with_terminal();
        let store = state.core.library_mut().store_mut(window).unwrap();
        let workspace = store.workspaces()[0].id();
        let second = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
        assert_eq!(store.selected_session_id(), Some(second));
        let row = |id, kind| SidebarRow {
            id,
            kind,
            name: String::new(),
            badge: 0,
        };
        state.sidebar_clicked(&row(first, RowKind::Session { selected: false }));
        let store = state.core.library().store(window).unwrap();
        assert_eq!(store.selected_session_id(), Some(first));
        state.sidebar_clicked(&row(workspace, RowKind::Workspace { expanded: true }));
        let store = state.core.library().store(window).unwrap();
        assert!(!store.workspace(workspace).unwrap().is_expanded);
        state.sidebar_clicked(&row(workspace, RowKind::Workspace { expanded: false }));
        let store = state.core.library().store(window).unwrap();
        assert!(store.workspace(workspace).unwrap().is_expanded);
    }
}
