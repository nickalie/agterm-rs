//! The main loop's state: the core and its on-screen windows, and the tasks that change them.
//!
//! RULE: every mutation runs as a main-loop task and ends with [`AppState::settle`], which lands the model's side
//! effects (the immediate saves and the events in the ring; the debounced saves, the soft closes' grace timers and the
//! windows' `tree.changed` debounces armed on the loop) and brings the windows (one per open store, each with its deck,
//! sidebar and title) in line with the model. GTK and libghostty handlers never touch the
//! state in place: they [`post`] a task, since libghostty may call back while the state is borrowed and a widget
//! must not be rebuilt inside its own handler.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Weak;
use std::time::Duration;

use agterm_core::auto_follow::AutoFollowTimer;
use agterm_core::events::{TREE_CHANGED_DEBOUNCE, TreeChangedTimer};
use agterm_core::model::PendingCloseTimer;
use agterm_core::persist::{DebouncedSave, arm_debounced_saves};
use agterm_core::sidebar_actions::SidebarAction;
use agterm_core::{CommitTimers, ControlError, Core, Dispatched, PaneReport, Resume};
use agterm_host::MainLoop;
use agterm_proto::command::SessionCmd;
use agterm_proto::{Command, Request};
use agterm_server::{LineStream, LineStreamLimits, LineStreamOwner, Reply, ReplySender};
use uuid::Uuid;

use crate::main_loop::GlibMainLoop;
use crate::quick::QuickPanel;
use crate::windows::Windows;

/// The bounds of an overlay job helper's connection (upstream's `overlayJobLimits`: a presentation frame's size).
const JOB_STREAM_LIMITS: LineStreamLimits = LineStreamLimits {
    max_line_bytes: 256 * 1024,
    max_pending_lines: 16,
    write_timeout: Duration::from_secs(5),
};

/// A helper connection as the core sends to it.
struct JobLines(LineStream);

impl agterm_core::JobChannel for JobLines {
    fn send(&self, line: Vec<u8>) -> bool {
        self.0.send(line)
    }

    fn shutdown(&self) {
        self.0.shutdown();
    }
}

/// Who waits for a deferred answer: a control connection, or a page's bridge request (by its token).
enum Waiter {
    Socket(ReplySender),
    #[cfg(feature = "html")]
    Page(u64),
}

impl Waiter {
    fn send(self, response: agterm_proto::Response) {
        match self {
            Self::Socket(sender) => sender.send(response),
            #[cfg(feature = "html")]
            Self::Page(token) => crate::html_overlay::answer(token, &response),
        }
    }
}

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

/// Where the timers of a commit land when they fire.
pub struct TimerHandlers<S> {
    /// A debounced save's quiet window elapsed.
    pub save: fn(&mut S, DebouncedSave),
    /// A soft close's grace ran out.
    pub finalize: fn(&mut S, PendingCloseTimer),
    /// A window's `tree.changed` debounce elapsed.
    pub tree_changed: fn(&mut S, TreeChangedTimer),
}

/// Arms what a commit leaves to the main loop: each debounced save fires after `SAVE_DEBOUNCE`, each soft close's
/// grace timer after its grace, each window's `tree.changed` after [`TREE_CHANGED_DEBOUNCE`]. A closed loop drops
/// them: the quit path writes everything and makes every pending close final anyway, and nobody reads the ring after.
pub fn arm_commit<S: 'static>(main_loop: &dyn MainLoop<S>, timers: &CommitTimers, handlers: &TimerHandlers<S>) {
    arm_debounced_saves(main_loop, &timers.debounced, handlers.save);
    for timer in timers.pending_closes.iter().copied() {
        let finalize = handlers.finalize;
        let _ = main_loop.post_after(timer.grace, Box::new(move |state: &mut S| finalize(state, timer)));
    }
    for timer in timers.tree_changed.iter().copied() {
        let fire = handlers.tree_changed;
        let _ = main_loop.post_after(TREE_CHANGED_DEBOUNCE, Box::new(move |state: &mut S| fire(state, timer)));
    }
}

/// The app state's handlers of a commit's timers.
const APP_TIMERS: TimerHandlers<AppState> = TimerHandlers {
    save: fire_debounced,
    finalize: finalize_pending_close,
    tree_changed: fire_tree_changed,
};

/// Plays a status sound. Linux has none of the named system sounds upstream plays, so every name, `default` and `beep`
/// included, sounds the display's alert.
fn play_status_sound(name: &str) {
    use gtk::prelude::DisplayExt;
    if let Some(display) = gtk::gdk::Display::default() {
        display.beep();
    } else {
        tracing::debug!(name, "no display to sound the status alert on");
    }
}

/// What the main loop owns.
pub struct AppState {
    pub core: Core,
    poster: Poster,
    /// The on-screen windows; `None` until the first activation (and in tests).
    windows: Option<Windows>,
    /// The quit save ran: nothing is committed, saved or re-rendered after it.
    quitting: bool,
    /// The app's one quick terminal panel.
    quick: QuickPanel,
}

impl AppState {
    #[must_use]
    pub fn new(core: Core) -> Self {
        Self {
            core,
            poster: detached_poster(),
            windows: None,
            quitting: false,
            quick: QuickPanel::default(),
        }
    }

    /// Connects the state to its main loop and its windows, and builds, renders and presents a window for every open
    /// store.
    pub fn attach(&mut self, poster: Poster, windows: Windows) {
        self.poster = poster;
        self.windows = Some(windows);
        self.settle();
    }

    /// Raises the frontmost window, its shown terminal focused.
    pub fn present(&self) {
        if let Some(windows) = self.windows.as_ref() {
            windows.present_frontmost(&self.core);
        }
    }

    /// The end of every mutation (module docs): commit, arm the loop's timers, re-render.
    pub fn settle(&mut self) {
        if self.quitting {
            return;
        }
        let timers = self.core.commit();
        if let Some(main_loop) = self.poster.upgrade() {
            arm_commit(&*main_loop, &timers, &APP_TIMERS);
        }
        // before the windows sync: a released page's panel leaves its parent first
        crate::html_overlay::settle(&mut self.core);
        let sounds = self.core.take_status_sounds();
        let banners = self.core.take_notifications();
        match self.windows.as_mut() {
            Some(windows) => {
                windows.sync(&mut self.core, &self.poster);
                self.quick.sync(&mut self.core, windows, &self.poster);
                for name in &sounds {
                    play_status_sound(name);
                }
                crate::notifications::deliver(windows.app(), &banners);
            }
            // no frontend to ask
            None => drop(self.core.take_window_requests()),
        }
        self.core.clear_pane_focus_requests();
        self.arm_auto_follow();
        self.arm_hud_timers();
        self.arm_job_expiries();
    }

    /// Arms the HUD auto-hides the core asked for since the last arm; each lands in [`Core::fire_hud_hide`].
    fn arm_hud_timers(&mut self) {
        let timers = self.core.take_hud_timers();
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        for timer in timers {
            let task = Box::new(move |state: &mut Self| {
                if !state.quitting && state.core.fire_hud_hide(timer) {
                    state.settle();
                }
            });
            if main_loop.post_after(timer.delay, task).is_err() {
                tracing::debug!("hud auto-hide dropped: the loop is closed");
            }
        }
    }

    /// The deck laid session `session`'s page out anew: a live HUD is measured against it again.
    pub fn report_hud_frames(&mut self, session: Uuid, frames: &agterm_core::hud::HudPaneFrames) {
        if !self.quitting && self.core.report_hud_frames(session, frames) {
            self.settle();
        }
    }

    /// Arms the auto-follow fires the core asked for since the last arm; each lands in [`AppState::fire_auto_follow`].
    pub fn arm_auto_follow(&mut self) {
        let timers = self.core.take_auto_follow_timers();
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        for timer in timers {
            let task = Box::new(move |state: &mut Self| state.fire_auto_follow(timer));
            if main_loop.post_after(timer.delay, task).is_err() {
                tracing::debug!("auto-follow fire dropped: the loop is closed");
            }
        }
    }

    /// An armed auto-follow delay elapsed. When the jump lands in the window holding the keyboard and no zoom covers
    /// it, the pane that set the status takes focus (upstream's `autoFollowed`); elsewhere only the selection moves.
    fn fire_auto_follow(&mut self, timer: AutoFollowTimer) {
        if self.quitting {
            return;
        }
        if let Some(followed) = self.core.fire_auto_follow(timer)
            && self.is_key_window(followed.window)
            && self.core.zoom_target(followed.window).is_none()
        {
            self.core
                .reveal_blocked_pane(followed.session, followed.indicator.as_ref());
        }
        self.settle();
    }

    /// Whether window `window` holds the keyboard: the frontmost window while the app's active window is focused.
    fn is_key_window(&self, window: Uuid) -> bool {
        use gtk::prelude::{GtkApplicationExt, GtkWindowExt};
        self.core.library().frontmost_window_id == Some(window)
            && self
                .windows
                .as_ref()
                .and_then(|windows| windows.app().active_window())
                .is_some_and(|active| active.is_active())
    }

    /// The window's exit-zoom control was clicked: its terminal zoom ends.
    pub fn exit_zoom(&mut self, window: Uuid) {
        self.core.set_zoom_target(window, None);
        self.settle();
    }

    /// The user answered window `window`'s picker `id`: a row, typed text, or a dismissal. A stale id changes nothing.
    pub fn resolve_pick(&mut self, window: Uuid, id: &str, outcome: agterm_proto::ControlPickResult) {
        if self.quitting {
            return;
        }
        self.core.resolve_pick(window, id, outcome);
        self.settle();
    }

    /// The user answered window `window`'s GUI ask `id`: a button, or Escape. A stale id changes nothing.
    pub fn answer_window_ask(&mut self, window: Uuid, id: &str, outcome: agterm_proto::ControlAskResult) {
        if self.quitting {
            return;
        }
        self.core.answer_window_ask(window, id, outcome);
        self.settle();
    }

    /// The user answered session `session`'s terminal ask `id`: a button, or Escape. A stale id changes nothing.
    pub fn answer_session_ask(&mut self, session: Uuid, id: &str, outcome: agterm_proto::ControlAskResult) {
        if self.quitting {
            return;
        }
        self.core.answer_session_ask(session, id, outcome);
        self.settle();
    }

    /// The user clicked session `session`'s terminal ask: the pane it covers becomes the split's focused pane, so the
    /// dialog takes the keyboard (upstream's `SessionAskInput.selectPane`).
    pub fn select_ask_pane(&mut self, session: Uuid) {
        let Some(store) = self.core.library_mut().store_for_session_mut(session) else {
            return;
        };
        let Some(pane) = store
            .session(session)
            .and_then(agterm_core::model::Session::ask_target_pane)
        else {
            return;
        };
        if store.set_split_focus(session, pane == agterm_core::model::OverlayPane::Right) {
            self.settle();
        }
    }

    /// Answers one control request and settles what it changed. A command waiting on the frontend (a terminal coming
    /// up, libghostty reporting a search count) answers later: its steps run as delayed main-loop tasks, each settled,
    /// while the connection's thread waits for the deferred reply.
    pub fn handle_request(&mut self, request: &Request) -> Reply {
        let dispatched = self.core.begin(request);
        let reply = match dispatched {
            Dispatched::Done(response) if request.cmd == Command::Session(SessionCmd::OverlayJobRun) && response.ok => {
                self.job_stream(response)
            }
            other => {
                self.settle();
                return self.reply(request, other);
            }
        };
        self.settle();
        reply
    }

    /// The reply of a dispatched request: its answer, or a deferred one its next step sends.
    fn reply(&self, request: &Request, dispatched: Dispatched) -> Reply {
        match dispatched {
            Dispatched::Done(response) => response.into(),
            Dispatched::Pending { after, resume } => {
                let (reply, sender) = Reply::deferred();
                self.schedule(request.cmd, after, resume, Waiter::Socket(sender));
                reply
            }
        }
    }

    /// Runs a request an HTML page sent through its bridge and answers the page's request `token`, as
    /// [`AppState::handle_request`] answers the socket (upstream's `HtmlBridgeDispatch`, the socket's entry point).
    #[cfg(feature = "html")]
    pub fn handle_page_request(&mut self, request: &Request, token: u64) {
        // a panic answers `ok:false` as the socket's handler does, rather than unwinding into the main loop
        let dispatched = panic::catch_unwind(AssertUnwindSafe(|| self.core.begin(request)));
        self.settle();
        match dispatched {
            Ok(Dispatched::Done(response)) => crate::html_overlay::answer(token, &response),
            Ok(Dispatched::Pending { after, resume }) => {
                self.schedule(request.cmd, after, resume, Waiter::Page(token));
            }
            Err(_) => {
                tracing::error!(cmd = %request.cmd, "control handler panicked on a page's request");
                crate::html_overlay::answer(token, &ControlError::HandlerPanicked(request.cmd).into());
            }
        }
    }

    /// The answer to a won overlay job claim, which keeps the connection (upstream's `adoptOverlayJobStream`): the
    /// helper's stream, registered with the core so it gets the launch context first, its frames and its close
    /// reported back as main-loop tasks.
    fn job_stream(&mut self, response: agterm_proto::Response) -> Reply {
        let Some(job) = response
            .result
            .as_ref()
            .and_then(|result| result.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            return response.into();
        };
        let stream = LineStream::new(JOB_STREAM_LIMITS);
        let serial = self
            .core
            .adopt_overlay_job_stream(&job, Box::new(JobLines(stream.clone())));
        let (lines, closes) = (self.poster.clone(), self.poster.clone());
        let (line_job, close_job) = (job.clone(), job);
        let owner = LineStreamOwner::new(
            stream,
            move |line| {
                let job = line_job.clone();
                post(&lines, move |state: &mut Self| {
                    state.core.overlay_job_line(&job, serial, &line);
                    state.settle();
                });
            },
            move || {
                post(&closes, move |state: &mut Self| {
                    state.core.overlay_job_closed(&close_job, serial);
                    state.settle();
                });
            },
        );
        Reply::streaming(response, Box::new(owner))
    }

    /// Arms the overlay job expiries the core asked for since the last arm.
    fn arm_job_expiries(&mut self) {
        let delays = self.core.take_overlay_job_expiries();
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        for delay in delays {
            let task = Box::new(|state: &mut Self| {
                if !state.quitting {
                    state.core.expire_overlay_jobs();
                    state.settle();
                }
            });
            if main_loop.post_after(delay, task).is_err() {
                tracing::debug!("overlay job expiry dropped: the loop is closed");
            }
        }
    }

    /// Runs `resume` after `after`; a closed loop drops it, and the dropped sender answers "shutting down".
    fn schedule(&self, cmd: Command, after: Duration, resume: Resume, sender: Waiter) {
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        let task = Box::new(move |state: &mut Self| state.resume(cmd, resume, sender));
        if main_loop.post_after(after, task).is_err() {
            tracing::debug!(%cmd, "deferred reply dropped: the loop is closed");
        }
    }

    /// One step of a waiting command; a panic in it answers `ok:false` as a handler's panic does.
    fn resume(&mut self, cmd: Command, resume: Resume, sender: Waiter) {
        let step = panic::catch_unwind(AssertUnwindSafe(|| resume(&mut self.core)));
        self.settle();
        match step {
            Ok(Dispatched::Done(response)) => sender.send(response),
            Ok(Dispatched::Pending { after, resume }) => self.schedule(cmd, after, resume, sender),
            Err(_) => {
                tracing::error!(%cmd, "control handler panicked in a deferred step");
                sender.send(ControlError::HandlerPanicked(cmd).into());
            }
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

    /// The shell of the pane carrying `token` ended: its pane closes by the pane's role now (upstream's per-pane
    /// `onExit`: the primary's promotes a split survivor or closes the session, the split's collapses the split, the
    /// scratch's closes the scratch). Nothing after the quit save, so terminals ending as they go close nothing.
    pub fn pane_exited(&mut self, session: Uuid, token: &str) {
        if !self.quitting && self.core.pane_exited(session, token) {
            self.settle();
        }
    }

    /// Keyboard focus moved into the pane carrying `token`, by the user or by the deck: a shown split's focus follows
    /// it, and the session's badge and delivered banners clear (see [`Core::pane_focused`]).
    pub fn pane_focused(&mut self, session: Uuid, token: &str) {
        if !self.quitting && self.core.pane_focused(session, token) {
            self.settle();
        }
    }

    /// Lands a pane's cwd or title report on the pane carrying `token`, wherever its session lives now (upstream's
    /// `applyPwd`/`applyTitle`): in a window's tree, or held by a pending soft close whose terminal still runs. Nothing
    /// is saved, as upstream.
    pub fn report_pane(&mut self, session: Uuid, token: &str, report: &PaneReport) {
        if self.core.report_pane(session, token, report) {
            self.settle();
        }
    }

    /// The user dragged session `session`'s split divider to `ratio`, or double-clicked it back to even: the session
    /// stores it, clamped, and saves (upstream's `applySplitRatio`).
    pub fn set_split_ratio(&mut self, session: Uuid, ratio: f64) {
        let applied = self
            .core
            .library_mut()
            .store_for_session_mut(session)
            .and_then(|store| store.apply_split_ratio(ratio, session));
        if applied.is_some() {
            self.settle();
        }
    }

    /// A sidebar gesture in window `window` ([`agterm_core::sidebar_actions`]); stands down while its modal slot is
    /// held.
    pub fn sidebar_action(&mut self, window: Uuid, action: SidebarAction) {
        if self.core.sidebar_action(window, action) {
            self.settle();
        }
    }

    /// The user opened or folded workspace `workspace`'s row in window `window`: the outline follows and the model
    /// persists it (upstream's outline expand and collapse callbacks, a genuine user toggle).
    pub fn sidebar_toggle_workspace(&mut self, window: Uuid, workspace: Uuid, expanded: bool) {
        let action = SidebarAction::SetWorkspaceExpanded { workspace, expanded };
        if !self.core.sidebar_action(window, action) {
            return;
        }
        if let Some(ui) = self.windows.as_mut().and_then(|windows| windows.get_mut(window)) {
            ui.set_workspace_expanded(workspace, expanded);
        }
        self.settle();
    }

    /// A drag opened (or, unlanded, folds back) workspace `workspace`'s row in window `window`, for view only.
    pub fn sidebar_spring_load(&mut self, window: Uuid, workspace: Uuid, open: bool) {
        if let Some(ui) = self.windows.as_mut().and_then(|windows| windows.get_mut(window)) {
            ui.set_workspace_expanded(workspace, open);
            self.settle();
        }
    }

    /// An inline rename began in window `window`'s sidebar: auto-follow waits until it ends.
    pub fn sidebar_rename_began(&mut self, window: Uuid) {
        self.core.suppress_auto_follow(window);
    }

    /// The inline rename in window `window`'s sidebar ended, renaming with `action` when committed: auto-follow
    /// resumes and the selected terminal takes the keyboard back.
    pub fn sidebar_rename_ended(&mut self, window: Uuid, action: Option<SidebarAction>) {
        self.core.resume_auto_follow(window);
        if let Some(action) = action {
            self.core.sidebar_action(window, action);
        }
        self.core.sidebar_action(window, SidebarAction::FocusActiveTerminal);
        self.settle();
    }

    /// The user dragged window `window`'s sidebar divider: its sidebar takes the new width.
    pub fn sidebar_resized(&mut self, window: Uuid) {
        let Some(width) = self
            .windows
            .as_ref()
            .and_then(|windows| windows.get(window))
            .and_then(crate::window::WindowUi::dragged_sidebar_width)
        else {
            return;
        };
        let Some(store) = self.core.library_mut().store_mut(window) else {
            return;
        };
        // the divider sits on whole pixels: a position the stored width rounds to is the model's own, not a drag
        if (store.sidebar().width.round() - width).abs() < f64::EPSILON {
            return;
        }
        store.set_sidebar_width(width);
        self.settle();
    }

    /// Window `window`'s on-screen state changed (full screen, maximized, minimized): recorded for `window.list`.
    pub fn window_state_changed(&mut self, window: Uuid) {
        if let Some(ui) = self.windows.as_ref().and_then(|windows| windows.get(window)) {
            self.core.set_window_state(window, Some(ui.window_state()));
        }
    }

    /// The user activated window `window`: it becomes frontmost (upstream's `WindowAccessor.reportFrontmost`).
    pub fn report_frontmost(&mut self, window: Uuid) {
        if self.quitting {
            return;
        }
        self.core.report_frontmost(window);
        self.settle();
    }

    /// The user closed window `window` (upstream's window `willClose`): its snapshot is written and its store closed,
    /// tearing its terminals down; the settle then drops the GTK window, and the app quits once none is open.
    pub fn close_requested(&mut self, window: Uuid) {
        if self.quitting {
            return;
        }
        self.core.close_window(window);
        self.settle();
    }
}

fn fire_debounced(state: &mut AppState, save: DebouncedSave) {
    if !state.quitting {
        state.core.fire_debounced(save);
    }
}

fn fire_tree_changed(state: &mut AppState, timer: TreeChangedTimer) {
    if !state.quitting {
        state.core.fire_tree_changed(timer);
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

    use agterm_core::events::{EventCursor, ReadOptions};
    use agterm_core::model::{NewSession, WindowLibrary};
    use agterm_core::persist::{LibraryPersistence, SAVE_DEBOUNCE};
    use agterm_core::{CoreConfig, CoreState};
    use agterm_host::fakes::FakeSurface;
    use agterm_host::{MainLoopClosed, MainTask};
    use agterm_proto::{AppIdentity, EventKind};

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
        let main_loop: Arc<RecordingLoop<AppState>> = Arc::new(RecordingLoop::new());
        let as_dyn: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
        state.poster = Arc::downgrade(&as_dyn);
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
        let main_loop: Arc<RecordingLoop<AppState>> = Arc::new(RecordingLoop::new());
        let as_dyn: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
        state.poster = Arc::downgrade(&as_dyn);

        let request = Request {
            cmd: Command::Session(agterm_proto::command::SessionCmd::Type),
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
        assert_eq!(reported.display_name(), "vim");
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
        state.close_requested(second);
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

    #[test]
    fn a_sidebar_rename_holds_auto_follow_until_it_ends() {
        let (mut state, window, first) = state_with_terminal();
        state.sidebar_rename_began(window);
        state.sidebar_rename_ended(
            window,
            Some(SidebarAction::RenameSession {
                session: first,
                name: "build".into(),
            }),
        );
        let store = state.core.library().store(window).unwrap();
        assert_eq!(store.session(first).unwrap().display_name(), "build");
    }

    #[test]
    fn a_sidebar_click_is_activity_and_an_armed_fire_follows_to_the_blocked_session() {
        use agterm_core::auto_follow::FollowConfig;
        use agterm_core::model::{AgentIndicator, AgentStatus};

        let (mut state, window, first) = state_with_terminal();
        let main_loop: Arc<RecordingLoop<AppState>> = Arc::new(RecordingLoop::new());
        let as_dyn: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
        state.poster = Arc::downgrade(&as_dyn);
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
        let fires: Vec<_> = main_loop
            .take()
            .into_iter()
            .filter(|(delay, _)| *delay == Some(Duration::ZERO))
            .collect();
        assert_eq!(fires.len(), 1);
        for (_, task) in fires {
            task(&mut state);
        }
        let store = state.core.library().store(window).unwrap();
        assert_eq!(store.selected_session_id(), Some(first));
    }
}
