//! The main loop's state: the core, and the tasks that change it. Ported from `agterm-gtk/src/state.rs`; it grows task
//! by task (the windows and their decks since Task 18).
//!
//! RULE: every mutation runs as a main-loop task and ends with [`AppState::settle`], which lands the model's side
//! effects (the immediate saves and the events in the ring; the debounced saves, the soft closes' grace timers and the
//! windows' `tree.changed` debounces armed on the loop) and records what the frontend must do with the app context
//! ([`FrontendWork`]). Handlers never touch the state in place: they [`post`] a task. The loop's consumer runs each
//! task with the state, then brings the windows in line with the model (`windows.rs`, the GTK build's settle-time
//! windows sync) and lands the frontend work (`app.rs`).

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::Weak;
use std::time::{Duration, Instant};

use agterm_app::remote::{InboundGate, PresentationViewer, ProcessTransport};
use agterm_core::app_actions::{ActionEffect, ActionOrigin, FrontendAction};
use agterm_core::auto_follow::AutoFollowTimer;
use agterm_core::events::{TREE_CHANGED_DEBOUNCE, TreeChangedTimer};
use agterm_core::keymap::TypingSurface;
use agterm_core::link_policy::{self, ClickOrigin, Route};
use agterm_core::model::{PendingCloseTimer, StatusKeystroke};
use agterm_core::persist::{DebouncedSave, arm_debounced_saves};
use agterm_core::sidebar_actions::SidebarAction;
use agterm_core::{CommitTimers, ControlError, Core, Dispatched, PaneReport, PaneSpawn, Resume};
use agterm_host::MainLoop;
use agterm_proto::command::{SessionCmd, ZmxCmd};
use agterm_proto::{Command, Request};
use agterm_server::{LineStream, LineStreamLimits, LineStreamOwner, Reply, ReplySender};
use uuid::Uuid;

use crate::instance::Activation;
use crate::keymap_dispatch::{Fired, KeyOrigin};
use crate::main_loop::GpuiMainLoop;

/// What a new window's first activation, while it may still be the focus the window manager grants on mapping it,
/// knows of the window's build ([`AppState::window_activated`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstFocus {
    /// Built when the core's [`frontmost_takes`](agterm_core::Core::frontmost_takes) stood at this count.
    Built { frontmost_takes: u64 },
    /// Built and then minimized by its own command (`window.new --minimized`), which handed frontmost to a window in
    /// view.
    Parked,
}

/// How long a quit a command asked for waits, so the command's reply is written first.
pub const LIVE_QUIT_DELAY: Duration = Duration::from_millis(250);

/// The bounds of an overlay job helper's connection (upstream's `overlayJobLimits`: a presentation frame's size).
const JOB_STREAM_LIMITS: LineStreamLimits = LineStreamLimits {
    max_line_bytes: 256 * 1024,
    max_pending_lines: 16,
    write_timeout: Duration::from_secs(5),
};

/// The bounds of a viewer's presentation stream (upstream's `presentationLimits`).
const PRESENTATION_LIMITS: LineStreamLimits = LineStreamLimits {
    max_line_bytes: agterm_core::remote::frames::MAX_FRAME_BYTES,
    max_pending_lines: agterm_core::remote::frames::MAX_PENDING_FRAMES,
    write_timeout: agterm_core::remote::WRITE_TIMEOUT,
};

/// An adopted streaming connection (an overlay job's helper, a viewer's presentation stream) as the core sends to it.
struct StreamLines(LineStream);

impl agterm_core::LineChannel for StreamLines {
    fn send(&self, line: Vec<u8>) -> bool {
        self.0.send(line)
    }

    fn shutdown(&self) {
        self.0.shutdown();
    }
}

/// Where handlers post tasks: the main loop, held weakly so a handler outliving it posts nothing.
pub type Poster = Weak<dyn MainLoop<AppState>>;

/// A poster that reaches no loop, until the real one is attached.
#[must_use]
pub fn detached_poster() -> Poster {
    Weak::<GpuiMainLoop<AppState>>::new()
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

/// What the tasks since the last landing left to the frontend, which needs the app context to do it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FrontendWork {
    /// Raise the frontmost window, with what each launch that asked handed over.
    pub activations: Vec<Activation>,
    /// Open these with the desktop's handler (a followed link).
    pub open_urls: Vec<String>,
    /// Reveal these in the file manager (a followed `file://` link, `session.reveal`, Reveal in Files).
    pub reveals: Vec<std::path::PathBuf>,
    /// Quit the app: the orderly shutdown, then GPUI's quit.
    pub quit: bool,
    /// What built-in actions left to the frontend (`keymap_actions`), each with the soft-close setting a delete
    /// confirmation names.
    pub actions: Vec<(FrontendAction, bool)>,
    /// The banners to post and withdraw, in the core's order ([`crate::notifications`]).
    pub banners: Vec<agterm_core::NotificationEffect>,
    /// The status sounds to play, and a posted batch's notification sound ([`crate::status_sound`]).
    pub sounds: Vec<String>,
    /// Show the first-launch welcome over the frontmost window ([`crate::welcome`]).
    pub welcome: bool,
    /// Open the Settings popup over this model window, or bring the one up to the front ([`crate::settings`]).
    pub settings: Option<Uuid>,
    /// Ask over the frontmost window whether to quit, with this message ([`crate::welcome::present_quit`]).
    pub quit_prompt: Option<String>,
    /// Show Reset Live Sessions…'s confirmation or refusal over the frontmost window ([`crate::zmx_runtime`]).
    pub live_reset: Option<crate::zmx_runtime::ResetAlert>,
}

/// Whether a terminal's desktop notification fired while its pane held its window's keyboard (upstream's first
/// responder check) and its window was the active one (`NSApp.isActive`), sampled when it fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NotifyFocus {
    pub pane: bool,
    pub active: bool,
}

/// What the main loop owns.
pub struct AppState {
    pub core: Core,
    poster: Poster,
    /// The quit save ran: nothing is committed, saved or re-rendered after it.
    quitting: bool,
    /// The window holding the keyboard, as the frontend reports it.
    key_window: Option<Uuid>,
    /// What [`AppState::take_frontend_work`] hands the frontend next.
    frontend: FrontendWork,
    /// The presentation heartbeat is armed on the loop.
    heartbeat_armed: bool,
    /// The remote tick (the attached rows' clients, reconnects and attaches) is armed on the loop.
    remote_tick_armed: bool,
    /// Panes the core seeded at their ask, waiting for the frontend to hand them the seed.
    seeded: Vec<PaneSpawn>,
    /// The terminal theme's side as last applied, and the desktop's (`appearance.rs`).
    pub(crate) appearance: crate::appearance::Appearance,
    /// The directories the core asked to reveal (`session.reveal`, Reveal in Files), for the frontend.
    revealed: Rc<RefCell<Vec<std::path::PathBuf>>>,
    /// The watch on `remotes.conf`; `None` until the launch starts it (and in tests).
    remotes_watcher: Option<crate::remotes_watcher::RemotesWatcher>,
}

/// The presentation bridges' reports ([`agterm_app::remote`]).
impl PresentationViewer for AppState {
    fn viewer_line(&mut self, key: agterm_core::remote::LinkKey, line: &[u8]) {
        if !self.quitting {
            self.core.viewer_line(key, line);
            self.settle();
        }
    }

    fn viewer_closed(&mut self, key: agterm_core::remote::LinkKey, reason: &str) {
        if !self.quitting {
            self.core.viewer_closed(key, reason);
            self.settle();
        }
    }
}

impl AppState {
    #[must_use]
    pub fn new(mut core: Core) -> Self {
        let revealed: Rc<RefCell<Vec<std::path::PathBuf>>> = Rc::default();
        let sink = Rc::clone(&revealed);
        core.set_reveal_handler(Box::new(move |path| sink.borrow_mut().push(path.to_owned())));
        Self {
            core,
            revealed,
            poster: detached_poster(),
            quitting: false,
            key_window: None,
            frontend: FrontendWork::default(),
            heartbeat_armed: false,
            remote_tick_armed: false,
            seeded: Vec::new(),
            appearance: crate::appearance::Appearance::default(),
            remotes_watcher: None,
        }
    }

    /// Starts following `remotes.conf` on disk (upstream's `RemotesWatcher`): every settled change reads it again,
    /// which enables or disables File ▸ Attach Remote and may post its problems banner. The watch moves with the config
    /// directory at the next settle. Once, at launch.
    pub fn watch_remotes(&mut self) {
        let poster = self.poster.clone();
        let watcher = crate::remotes_watcher::RemotesWatcher::new(move || {
            post(&poster, |state: &mut Self| {
                if !state.quitting {
                    state.core.refresh_remotes(true);
                    state.settle();
                }
            });
        });
        match watcher {
            Ok(mut watcher) => {
                watcher.follow(self.core.watched_remotes_path());
                self.remotes_watcher = Some(watcher);
            }
            Err(error) => tracing::error!(%error, "remotes.conf: not followed; edits are seen at the next launch"),
        }
    }

    /// Connects the state to its main loop, then settles.
    pub fn attach(&mut self, poster: Poster) {
        // the attached rows' presentation bridges run as children reporting back through the loop
        self.core
            .set_presentation_transport(std::rc::Rc::new(ProcessTransport::new(poster.clone())));
        self.poster = poster;
        self.settle();
    }

    /// Where tasks for this state are posted.
    #[must_use]
    pub fn poster(&self) -> &Poster {
        &self.poster
    }

    /// Whether the quit save ran.
    #[must_use]
    pub fn is_quitting(&self) -> bool {
        self.quitting
    }

    /// What the tasks since the last call left to the frontend.
    pub fn take_frontend_work(&mut self) -> FrontendWork {
        let revealed = std::mem::take(&mut *self.revealed.borrow_mut());
        self.frontend.reveals.extend(revealed);
        std::mem::take(&mut self.frontend)
    }

    /// Asks the frontend to confirm a user's quit with `message` (the app menu's Quit).
    pub(crate) fn ask_quit(&mut self, message: String) {
        self.frontend.quit_prompt = Some(message);
    }

    /// Asks the frontend to show Reset Live Sessions…'s `alert` over the frontmost window.
    pub(crate) fn ask_live_reset(&mut self, alert: crate::zmx_runtime::ResetAlert) {
        self.frontend.live_reset = Some(alert);
    }

    /// Asks the frontend to quit: SIGTERM, the last window closing, a command's quit.
    pub fn request_quit(&mut self) {
        self.frontend.quit = true;
    }

    /// Another launch asked for the app (the bus's `Activate`): the frontmost window is raised with what it handed
    /// over.
    pub fn activation_requested(&mut self, activation: Activation) {
        if !self.quitting {
            self.frontend.activations.push(activation);
        }
    }

    /// Window `window` took the keyboard, or (`None`) no window of the app holds it.
    pub fn set_key_window(&mut self, window: Option<Uuid>) {
        self.key_window = window;
    }

    /// Window `window` lost the keyboard: no window of the app holds it, unless another took it already.
    pub fn key_window_lost(&mut self, window: Uuid) {
        if self.key_window == Some(window) {
            self.key_window = None;
        }
    }

    /// The window holding the keyboard, as the frontend reported it.
    #[must_use]
    pub fn key_window(&self) -> Option<Uuid> {
        self.key_window
    }

    /// Window `window`'s on-screen state changed (full screen, maximized, its display): recorded for `window.list`.
    pub fn window_state_changed(&mut self, window: Uuid, state: agterm_core::WindowState) {
        if !self.quitting {
            self.core.set_window_state(window, Some(state));
        }
    }

    /// A session pane's terminal is ready to spawn: it spawns now with its seed, or once its paced launch turn comes
    /// (see [`Core::request_pane_spawn`]).
    pub fn pane_spawn_ready(&mut self, session: Uuid, token: &str) {
        if self.quitting {
            return;
        }
        if let Some(seed) = self.core.request_pane_spawn(session, token, Instant::now()) {
            self.seeded.push(PaneSpawn {
                session,
                token: token.to_owned(),
                seed,
            });
        }
        self.settle();
    }

    /// The panes to spawn now (the GTK build's `spawn_paced_panes`): those seeded at their ask, then the launch's paced
    /// grants, every window's selected session jumping the queue (it is on screen); one timer is armed for the next
    /// paced grant.
    pub fn take_spawns(&mut self) -> Vec<PaneSpawn> {
        let mut spawns = std::mem::take(&mut self.seeded);
        if self.quitting {
            return spawns;
        }
        let now = Instant::now();
        let library = self.core.library();
        let selected: Vec<Uuid> = library
            .open_ids()
            .into_iter()
            .filter_map(|id| {
                library
                    .store(id)
                    .and_then(agterm_core::model::AppStore::selected_session_id)
            })
            .collect();
        for session in selected {
            self.core.expedite_pane_spawns(session, now);
        }
        spawns.extend(self.core.take_spawn_grants());
        if let Some(delay) = self.core.take_spawn_wake(now)
            && let Some(main_loop) = self.poster.upgrade()
        {
            let task = Box::new(|state: &mut Self| {
                if !state.quitting {
                    state.core.spawn_wake(Instant::now());
                    state.settle();
                }
            });
            if main_loop.post_after(delay, task).is_err() {
                tracing::debug!("launch spawn wake dropped: the loop is closed");
            }
        }
        spawns
    }

    /// A link clicked in a terminal (the GTK build's `follow_link_in`): through the link policy as a click from
    /// `origin`, under the link-open setting; a session overlay opens over its session, the rest goes to the desktop.
    pub fn follow_link(&mut self, raw: &str, origin: ClickOrigin) {
        let mode = self.core.settings().effective_link_open_mode();
        match link_policy::route(raw, mode, origin) {
            Route::Overlay { url, session } => {
                let shown = url::Url::parse(&url).is_ok_and(|parsed| self.core.open_link_overlay(parsed, session));
                if shown {
                    self.settle();
                } else {
                    self.frontend.open_urls.push(url);
                }
            }
            Route::Browser(url) => self.frontend.open_urls.push(url),
            Route::Reveal(path) => self.frontend.reveals.push(path),
            Route::Ignore => tracing::debug!(raw, "link click ignored by the link policy"),
        }
    }

    /// A key the user pressed in the pane carrying `token` (the GTK build's `connect_keystroke`): the session saw
    /// user activity, and the keystroke clears its status glyph when the glyph's rule says so.
    pub fn pane_keystroke(&mut self, session: Uuid, token: &str, keystroke: StatusKeystroke) {
        self.core.note_user_activity(session);
        self.arm_auto_follow();
        if self.core.clear_status_on_keystroke(session, token, keystroke) {
            self.settle();
        }
    }

    /// A key the user pressed in the quick terminal: user activity in the active window (upstream's quick-terminal
    /// `onUserInput`), so an idle auto-follow cannot reshuffle the selection behind the panel while the user types.
    pub fn quick_keystroke(&mut self) {
        self.core.note_quick_input();
        self.arm_auto_follow();
    }

    /// A key the user pressed in one of `session`'s overlay terminals (the GTK build's overlay `connect_keystroke`):
    /// user activity for auto-follow.
    pub fn overlay_keystroke(&mut self, session: Uuid) {
        self.core.note_user_activity(session);
        self.arm_auto_follow();
    }

    /// A terminal's desktop notification, suppressed while its pane holds the keyboard of the active window
    /// (upstream's `NotificationManager.notify`).
    pub fn terminal_notification(&mut self, session: Uuid, token: &str, title: &str, body: &str, focus: NotifyFocus) {
        if self.quitting {
            return;
        }
        if !agterm_core::should_deliver(focus.pane, focus.active) {
            tracing::info!(%session, "notify: the firing pane is focused; suppressed");
            return;
        }
        if self.core.terminal_notification(session, token, title, body) {
            self.settle();
        }
    }

    /// The end of every mutation (module docs): commit, arm the loop's timers, record the frontend's work.
    pub fn settle(&mut self) {
        if self.quitting {
            return;
        }
        let timers = self.core.commit();
        if let Some(main_loop) = self.poster.upgrade() {
            arm_commit(&*main_loop, &timers, &APP_TIMERS);
        }
        if let Some(quit) = self.core.take_live_quit() {
            self.schedule_live_quit(quit);
        }
        crate::keymap_dispatch::sync(&self.core, &self.poster);
        self.take_alerts();
        if let Some(watcher) = &mut self.remotes_watcher {
            watcher.follow(self.core.watched_remotes_path());
        }
        // the window requests and pane focus requests wait for the windows' sync after the task (`windows.rs`)
        self.arm_auto_follow();
        self.arm_hud_timers();
        self.arm_job_expiries();
        self.arm_remote_tick();
    }

    /// Hands the frontend the status sounds and the banners the core queued; a batch posting banners sounds the
    /// configured notification sound after the status sounds, through the same throttled bell.
    fn take_alerts(&mut self) {
        self.frontend.sounds.extend(self.core.take_status_sounds());
        let banners = self.core.take_notifications();
        if let Some(sound) = crate::notifications::banner_sound(&banners, self.core.settings()) {
            self.frontend.sounds.push(sound.to_owned());
        }
        self.frontend.banners.extend(banners);
    }

    /// Asks the frontend to show the first-launch welcome.
    pub(crate) fn show_welcome(&mut self) {
        self.frontend.welcome = true;
    }

    /// Asks the frontend to open the Settings popup over model window `window`, or bring the one up to the front (the
    /// app menu's Settings…, run in its own window).
    pub fn open_settings(&mut self, window: Uuid) {
        if !self.quitting {
            self.frontend.settings = Some(window);
        }
    }

    /// Arms the remote tick when the core asks for one and none is armed (upstream's `startRemoteTick`); it runs
    /// [`Core::remote_tick`] and settles, which re-arms it while anything remote is up.
    fn arm_remote_tick(&mut self) {
        if self.remote_tick_armed {
            return;
        }
        let Some(delay) = self.core.remote_wake() else {
            return;
        };
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        let task = Box::new(|state: &mut Self| {
            state.remote_tick_armed = false;
            if state.quitting {
                return;
            }
            state.core.remote_tick(Instant::now());
            state.settle();
        });
        if main_loop.post_after(delay, task).is_ok() {
            self.remote_tick_armed = true;
        }
    }

    /// The network came back or the machine woke ([`crate::remote::observe_links`]): every waiting remote pane and
    /// dropped stream retries now, not on its backoff.
    pub fn retry_remote_links(&mut self) {
        if !self.quitting {
            self.core.retry_remote_links_now(Instant::now());
            self.settle();
        }
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
    fn arm_auto_follow(&mut self) {
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

    /// Whether window `window` holds the keyboard: the frontmost window, focused.
    fn is_key_window(&self, window: Uuid) -> bool {
        self.key_window == Some(window) && self.core.library().frontmost_window_id == Some(window)
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

    /// A keymap chord fired in a window (or the quick terminal's panel, which acts on the frontmost window): a built-in
    /// runs through the core, its frontend half lands with the app ([`FrontendWork::actions`]); a custom command runs
    /// in the terminal typed in.
    pub fn keymap_fired(&mut self, origin: KeyOrigin, fired: Fired, typing: Option<TypingSurface>) {
        if self.quitting {
            return;
        }
        let window = match origin {
            KeyOrigin::Window(window) => Some(window),
            KeyOrigin::Quick => self.core.library().active_window_id(),
        };
        match fired {
            Fired::Builtin(action) => {
                let Some(window) = window else { return };
                let origin = ActionOrigin {
                    window,
                    quick: origin == KeyOrigin::Quick,
                };
                if let ActionEffect::Frontend(action) = self.core.perform_builtin(origin, action) {
                    self.frontend_action(action);
                }
            }
            Fired::Command(command) => self.core.run_custom_command_from_keybind(&command, window, typing),
        }
        self.settle();
    }

    /// Leaves the frontend half of a built-in action (a palette row's, a chord's) to the landing
    /// ([`FrontendWork::actions`]), with the soft-close setting a delete confirmation names.
    pub fn frontend_action(&mut self, action: FrontendAction) {
        let close_grace = self.core.settings().close_grace_undo_enabled.unwrap_or(true);
        self.frontend.actions.push((action, close_grace));
    }

    /// A window dialog committed: `window.rename` or `window.delete` for window `window`.
    pub fn window_command(&mut self, cmd: agterm_proto::command::WindowCmd, window: Uuid, name: Option<String>) {
        if self.quitting {
            return;
        }
        let response = self
            .core
            .dispatch(&crate::keymap_actions::window_request(cmd, window, name));
        if !response.ok {
            tracing::info!(error = ?response.error, "window dialog refused");
        }
        self.settle();
    }

    /// Answers one control request and settles what it changed. A command waiting on the frontend (a terminal coming
    /// up, a search count) answers later: its steps run as delayed main-loop tasks, each settled, while the
    /// connection's thread waits for the deferred reply.
    pub fn handle_request(&mut self, request: &Request) -> Reply {
        let dispatched = self.core.begin(request);
        let reply = match dispatched {
            Dispatched::Done(response) if request.cmd == Command::Session(SessionCmd::OverlayJobRun) && response.ok => {
                self.job_stream(response)
            }
            Dispatched::Done(response) if request.cmd == Command::Zmx(ZmxCmd::Present) && response.ok => {
                self.presentation_stream(response)
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
                self.schedule(request.cmd, after, resume, sender);
                reply
            }
        }
    }

    /// The answer to a won overlay job claim, which keeps the connection (upstream's `adoptOverlayJobStream`): the
    /// helper's stream, registered with the core so it gets the launch context first, its frames and its close
    /// reported back as main-loop tasks.
    fn job_stream(&mut self, response: agterm_proto::Response) -> Reply {
        let Some(job) = result_id(&response) else {
            return response.into();
        };
        let stream = LineStream::new(JOB_STREAM_LIMITS);
        let serial = self
            .core
            .adopt_overlay_job_stream(&job, Box::new(StreamLines(stream.clone())));
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

    /// The answer to an accepted `zmx.present`, which keeps the connection (upstream's `adoptPresentationStream`): the
    /// viewer's stream, registered with the core, its lines and close reported back as main-loop tasks. The reader
    /// stops reading while [`INBOUND_LIMIT`](agterm_core::remote::INBOUND_LIMIT) lines wait for the loop, so a fast or
    /// faulty peer backs up into its own socket; a stream that sends no hello within the deadline is closed.
    fn presentation_stream(&mut self, response: agterm_proto::Response) -> Reply {
        let Some(session) = result_id(&response) else {
            return response.into();
        };
        let stream = LineStream::new(PRESENTATION_LIMITS);
        let serial = self
            .core
            .adopt_presentation_stream(&session, Box::new(StreamLines(stream.clone())));
        if let Some(main_loop) = self.poster.upgrade() {
            let deadline = Box::new(move |state: &mut Self| {
                if !state.quitting {
                    state.core.presentation_hello_deadline(serial);
                }
            });
            if main_loop
                .post_after(agterm_core::remote::HELLO_DEADLINE, deadline)
                .is_err()
            {
                tracing::debug!("presentation hello deadline dropped: the loop is closed");
            }
        }
        self.arm_presentation_heartbeat();
        let owner = LineStreamOwner::new(
            stream,
            presentation_reader(self.poster.clone(), serial),
            presentation_closer(self.poster.clone(), serial),
        );
        Reply::streaming(response, Box::new(owner))
    }

    /// Arms the presentation heartbeat unless it is armed (upstream's `startPresentationHeartbeat`); it re-arms itself
    /// while a stream is up.
    fn arm_presentation_heartbeat(&mut self) {
        if self.heartbeat_armed {
            return;
        }
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        let task = Box::new(|state: &mut Self| {
            state.heartbeat_armed = false;
            if state.quitting {
                return;
            }
            let streams_up = state.core.presentation_heartbeat();
            state.settle();
            if streams_up {
                state.arm_presentation_heartbeat();
            }
        });
        if main_loop
            .post_after(agterm_core::remote::HEARTBEAT_INTERVAL, task)
            .is_ok()
        {
            self.heartbeat_armed = true;
        }
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
    fn schedule(&self, cmd: Command, after: Duration, resume: Resume, sender: ReplySender) {
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        let task = Box::new(move |state: &mut Self| state.resume(cmd, resume, sender));
        if main_loop.post_after(after, task).is_err() {
            tracing::debug!(%cmd, "deferred reply dropped: the loop is closed");
        }
    }

    /// One step of a waiting command; a panic in it answers `ok:false` as a handler's panic does.
    fn resume(&mut self, cmd: Command, resume: Resume, sender: ReplySender) {
        let step = panic::catch_unwind(AssertUnwindSafe(|| resume(&mut self.core)));
        self.settle();
        match step {
            Ok(Dispatched::Done(response)) => sender.send(response),
            Ok(Dispatched::Pending { after, resume }) => self.schedule(cmd, after, resume, sender),
            Err(_) => {
                tracing::error!(%cmd, "control handler panicked in a deferred step");
                sender.send(agterm_proto::Response::from(ControlError::HandlerPanicked(cmd)));
            }
        }
    }

    /// Quits the app a moment after a command asked for it (`zmx.reset`), so the command's reply is written first
    /// (upstream's connection thread terminates once the reply is out).
    fn schedule_live_quit(&self, quit: agterm_core::LiveQuit) {
        tracing::info!(relaunch = quit.relaunch, "quitting for the live sessions reset");
        let Some(main_loop) = self.poster.upgrade() else {
            return;
        };
        if main_loop
            .post_after(LIVE_QUIT_DELAY, Box::new(Self::request_quit))
            .is_err()
        {
            tracing::debug!("the live sessions reset quit was dropped: the loop is closed");
        }
    }

    /// The quit path's save (upstream's `applicationWillTerminate`), once, before any terminal is torn down: every
    /// pending close made final and every open window and the index written. Nothing is saved after it, so the
    /// shells ending as their terminals go do not close their sessions on disk.
    pub fn save_on_quit(&mut self) {
        if self.quitting {
            return;
        }
        // the capture reads the panes' foreground programs, so it runs while every terminal still lives
        self.core.capture_on_exit();
        if !self.core.save_all() {
            tracing::error!("the quit save did not write everything");
        }
        self.core.shutdown_presentation_streams();
        self.core.shutdown_remote_presentations();
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
    /// held, a committed rename aside.
    pub fn sidebar_action(&mut self, window: Uuid, action: SidebarAction) {
        if self.core.sidebar_action(window, action) {
            self.settle();
        }
    }

    /// The user opened or folded workspace `workspace`'s row in window `window`: the outline follows and the model
    /// persists it (upstream's outline expand and collapse callbacks, a genuine user toggle). The outline follows
    /// through the store's per-row request, which the sidebar takes on its next sync (the GTK build set the row on its
    /// window directly).
    pub fn sidebar_toggle_workspace(&mut self, window: Uuid, workspace: Uuid, expanded: bool) {
        let action = SidebarAction::SetWorkspaceExpanded { workspace, expanded };
        if !self.core.sidebar_action(window, action) {
            return;
        }
        if let Some(store) = self.core.library_mut().store_mut(window) {
            store.set_workspace_expanded_in_outline(workspace, expanded);
        }
        self.settle();
    }

    /// The user dragged window `window`'s sidebar divider to `width`: its sidebar takes it, clamped, and saves. The
    /// divider sits on whole pixels: a width the stored one rounds to is the model's own, not a move.
    pub fn sidebar_resized(&mut self, window: Uuid, width: f64) {
        let Some(store) = self.core.library_mut().store_mut(window) else {
            return;
        };
        let width = width.round();
        if (store.sidebar().width.round() - width).abs() < f64::EPSILON {
            return;
        }
        store.set_sidebar_width(width);
        self.settle();
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

    /// Window `window` took the keyboard: it is the key window and becomes frontmost, except on the focus the window
    /// manager grants a new window when it maps it (`first`, [`FirstFocus`]) while another open window is frontmost
    /// and either a command took frontmost since the window was built or the window was built parked: that focus
    /// arrives after the open, so after such a command, which keeps frontmost, as on macOS, where a new window is key
    /// before the open returns. Frontmost the model set itself (a restore, a reopen) does not hold the window back.
    pub fn window_activated(&mut self, window: Uuid, first: Option<FirstFocus>) {
        self.set_key_window(Some(window));
        let held_back = match first {
            Some(FirstFocus::Built { frontmost_takes }) => self.core.frontmost_takes() != frontmost_takes,
            Some(FirstFocus::Parked) => true,
            None => false,
        };
        let library = self.core.library();
        if held_back
            && let Some(frontmost) = library.frontmost_window_id
            && frontmost != window
            && library.is_open(frontmost)
        {
            tracing::debug!(%window, %frontmost, "a new window's first focus came after a command took frontmost");
            return;
        }
        self.report_frontmost(window);
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
    /// tearing its terminals down.
    pub fn close_requested(&mut self, window: Uuid) {
        if self.quitting {
            return;
        }
        self.core.close_window(window);
        self.settle();
    }
}

/// The `id` field of a response's result.
fn result_id(response: &agterm_proto::Response) -> Option<String> {
    response
        .result
        .as_ref()
        .and_then(|result| result.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// A presentation stream's line handler: each line is a main-loop task, at most `INBOUND_LIMIT` waiting at once.
fn presentation_reader(poster: Poster, serial: u64) -> impl FnMut(Vec<u8>) + Send + 'static {
    let gate = InboundGate::new(agterm_core::remote::INBOUND_LIMIT);
    move |line| {
        gate.acquire();
        let released = gate.clone();
        let task = move |state: &mut AppState| {
            if !state.quitting {
                state.core.presentation_line(serial, &line);
                state.settle();
            }
            released.release();
        };
        let queued = poster
            .upgrade()
            .is_some_and(|main_loop| main_loop.post(Box::new(task)).is_ok());
        if !queued {
            gate.release();
        }
    }
}

/// A presentation stream's close handler.
fn presentation_closer(poster: Poster, serial: u64) -> impl FnOnce() + Send + 'static {
    move || {
        post(&poster, move |state: &mut AppState| {
            if !state.quitting {
                state.core.presentation_closed(serial);
                state.settle();
            }
        });
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
mod tests;
