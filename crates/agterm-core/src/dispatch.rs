//! The core state owner and the command dispatcher: one request in, one response out, on the main thread.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::time::Duration;

use agterm_proto::command::{EventsCmd, SessionCmd};
use agterm_proto::{
    AppIdentity, Args, Command, ConformanceProfile, ControlResult, ControlTree, PARITY_LEVEL, Request, Response,
    VersionResult,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::commands;
use crate::error::ControlError;
use crate::events::{ControlEvents, EventRing, ReadOptions, TreeChangedTimer};
use crate::model::PendingCloseTimer;
use crate::model::{AppStore, WindowLibrary};
use crate::persist::{AppSettings, DebouncedSave, LibraryPersistence};
use crate::projection::{TreeContext, control_tree};
use crate::resolve::Resolver;
use crate::windows::{WindowRequest, WindowState};
use crate::zoom::SurfaceId;

/// What the app tells the core about itself at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreConfig {
    /// The serving build, reported by `version` and `tree.app`.
    pub app: AppIdentity,
    /// The conformance capabilities this host claims (`conformance/docs/rust-build.md`).
    pub capabilities: Vec<String>,
}

/// The model the core starts with: the windows and their stores, the settings, and the persistence that loaded them.
#[derive(Debug, Default)]
pub struct CoreState {
    pub library: WindowLibrary,
    pub settings: AppSettings,
    /// `None` keeps the model in memory only (tests).
    pub persistence: Option<LibraryPersistence>,
}

/// The main-loop timers a [`Core::commit`] leaves to arm.
#[derive(Debug, Default)]
pub struct CommitTimers {
    /// The debounced snapshot writes, for [`Core::fire_debounced`] after
    /// [`SAVE_DEBOUNCE`](crate::persist::SAVE_DEBOUNCE).
    pub debounced: Vec<DebouncedSave>,
    /// The soft closes' grace timers, for [`Core::finalize_pending_close`] after each one's grace.
    pub pending_closes: Vec<PendingCloseTimer>,
    /// The windows' debounced `tree.changed`, for [`Core::fire_tree_changed`] after
    /// [`TREE_CHANGED_DEBOUNCE`](crate::events::TREE_CHANGED_DEBOUNCE).
    pub tree_changed: Vec<TreeChangedTimer>,
}

/// The next step of a command that waits on the frontend: run on the main loop once its delay passed.
pub type Resume = Box<dyn FnOnce(&mut Core) -> Dispatched + Send>;

/// A request's answer, or the step that will produce it once the frontend caught up (a terminal coming up, libghostty
/// reporting a search count), as upstream's handlers await short main-actor polls.
pub enum Dispatched {
    /// The answer.
    Done(Response),
    /// Not answered yet: run `resume` on the main loop after `after`, and answer with what it returns.
    Pending { after: Duration, resume: Resume },
}

impl Dispatched {
    /// An answer from a handler's outcome.
    pub(crate) fn done(outcome: Result<ControlResult, ControlError>) -> Self {
        Self::Done(
            outcome
                .and_then(|result| result_of(&result))
                .unwrap_or_else(Response::from),
        )
    }

    /// Runs the pending steps at once, without waiting out their delays: the answer a caller gets that cannot wait
    /// (tests over fakes, which change only when the test changes them).
    #[must_use]
    pub fn finish_now(mut self, core: &mut Core) -> Response {
        loop {
            match self {
                Self::Done(response) => return response,
                Self::Pending { resume, .. } => self = resume(core),
            }
        }
    }
}

impl fmt::Debug for Dispatched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Done(response) => f.debug_tuple("Done").field(response).finish(),
            Self::Pending { after, .. } => f.debug_struct("Pending").field("after", after).finish_non_exhaustive(),
        }
    }
}

/// The frontend's file-manager reveal of a directory (`session.reveal`).
struct RevealHandler(Box<dyn Fn(&Path)>);

impl fmt::Debug for RevealHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RevealHandler")
    }
}

/// The single owner of the app state. Lives on the main thread; every control request reaches it through
/// [`Core::dispatch`].
#[derive(Debug)]
pub struct Core {
    app: AppIdentity,
    capabilities: Vec<String>,
    library: WindowLibrary,
    settings: AppSettings,
    persistence: Option<LibraryPersistence>,
    /// The app run's event ring, fed by every commit.
    events: ControlEvents,
    /// The frontend's on-screen windows, by window id.
    window_states: HashMap<Uuid, WindowState>,
    /// What window commands left for the frontend's windows, in order, until it takes them.
    window_requests: Vec<WindowRequest>,
    reveal_handler: Option<RevealHandler>,
    /// The status sounds commands asked the frontend to play, in order, until it takes them.
    status_sounds: Vec<String>,
    /// Each window's terminal zoom: the one surface it fills itself with (upstream's per-window
    /// `TerminalZoomController.target`).
    zoom: HashMap<Uuid, SurfaceId>,
    /// Sessions whose focused pane a command asked to get the keyboard again, even unchanged, until the frontend's
    /// next render (upstream's `session.focus` grabs the pane whatever the model held).
    pane_focus_requests: HashSet<Uuid>,
    /// The app's one quick terminal ([`commands::quick`]).
    pub(crate) quick: commands::quick::QuickState,
    /// Every window's dashboard ([`commands::dashboard`]).
    pub(crate) dashboards: commands::dashboard::Dashboards,
    /// Every window's auto-follow ([`crate::auto_follow`]).
    pub(crate) auto_follow: crate::auto_follow::AutoFollow,
    /// The HUD commands' inputs and armed auto-hides ([`commands::hud`]).
    pub(crate) hud: commands::hud::HudRuntime,
    /// The desktop notifications commands and terminals asked for, in order, until the frontend takes them
    /// ([`commands::notify`]).
    pub(crate) notifications: Vec<commands::notify::NotificationEffect>,
}

/// The `tree` result: `{"tree": {...}}`.
#[derive(Serialize)]
struct TreeResult {
    tree: ControlTree,
}

/// Foundation's context for data that does not parse as JSON (swift-foundation's `JSONDecoder.decode`), whatever the
/// syntax error; upstream's socket answers it for a line that is not JSON at all.
const NOT_JSON: &str = "The given data was not valid JSON.";

/// Decodes one request line (newline excluded), as upstream's `handleConnection` decodes a `ControlRequest`
/// (`agterm/Control/ControlServer.swift`).
///
/// # Errors
///
/// [`ControlError::InvalidRequest`] for anything that is not a well-typed request object naming a known command. Its
/// context is the decoder's: Foundation's own sentence for a line that is not JSON and for an unknown `cmd`, serde's
/// message for the rest (a missing `cmd`, a value of the wrong type, a non-object). An unknown `cmd` wins over every
/// other defect, as Foundation decodes `cmd` first; that keeps the command an old app does not know named in the
/// error, whatever else the newer client sent.
pub fn decode_request(line: &[u8]) -> Result<Request, ControlError> {
    let value: Value = serde_json::from_slice(line).map_err(|_| ControlError::InvalidRequest(NOT_JSON.to_owned()))?;
    if let Some(Value::String(cmd)) = value.get("cmd")
        && let Err(unknown) = cmd.parse::<Command>()
    {
        return Err(ControlError::InvalidRequest(unknown.to_string()));
    }
    // from a parsed value, serde's messages carry no line and column, as Foundation's context carries none
    serde_json::from_value(value).map_err(|error| ControlError::InvalidRequest(error.to_string()))
}

fn result_of(value: &impl Serialize) -> Result<Response, ControlError> {
    serde_json::to_value(value)
        .map(Response::with_result)
        .map_err(|_| ControlError::Encode)
}

impl Core {
    /// A core over `state`.
    #[must_use]
    pub fn new(config: CoreConfig, state: CoreState) -> Self {
        Self {
            app: config.app,
            capabilities: config.capabilities,
            library: state.library,
            settings: state.settings,
            persistence: state.persistence,
            events: ControlEvents::default(),
            window_states: HashMap::new(),
            window_requests: Vec::new(),
            reveal_handler: None,
            status_sounds: Vec::new(),
            zoom: HashMap::new(),
            pane_focus_requests: HashSet::new(),
            quick: commands::quick::QuickState::default(),
            dashboards: commands::dashboard::Dashboards::default(),
            auto_follow: crate::auto_follow::AutoFollow::default(),
            hud: commands::hud::HudRuntime::default(),
            notifications: Vec::new(),
        }
    }

    /// Whether this host claims the conformance capability `name` (`window-geometry` decides whether `window.move`
    /// can act).
    #[must_use]
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|claimed| claimed == name)
    }

    /// Takes what window commands asked of the on-screen windows since the last take, in order. The frontend applies
    /// them after building the windows the same commit opened.
    pub fn take_window_requests(&mut self) -> Vec<WindowRequest> {
        std::mem::take(&mut self.window_requests)
    }

    /// Takes the status sounds commands asked for since the last take, in order (`session.status --sound`, the
    /// blocked-status default). The frontend plays them after the commit.
    pub fn take_status_sounds(&mut self) -> Vec<String> {
        std::mem::take(&mut self.status_sounds)
    }

    /// The surface window `id` is zoomed to, if any.
    #[must_use]
    pub fn zoom_target(&self, id: Uuid) -> Option<SurfaceId> {
        self.zoom.get(&id).copied()
    }

    /// Sets or clears window `id`'s zoom (the frontend's exit-zoom control clears it); a zoom replaces the window's
    /// dashboard. Commit afterwards.
    pub fn set_zoom_target(&mut self, id: Uuid, target: Option<SurfaceId>) {
        match target {
            Some(target) => {
                commands::dashboard::close(self, id);
                self.zoom.insert(id, target)
            }
            None => self.zoom.remove(&id),
        };
    }

    /// Whether a command asked session `session`'s focused pane to take the keyboard since the frontend's last render:
    /// its deck then grabs the pane even when it already gave it focus, as focus may have moved out of the terminals.
    #[must_use]
    pub fn pane_focus_requested(&self, session: Uuid) -> bool {
        self.pane_focus_requests.contains(&session)
    }

    /// Forgets the pane focus requests once the frontend rendered them; a request for a session no deck showed lapses,
    /// as upstream's grab does nothing for a session that is not selected.
    pub fn clear_pane_focus_requests(&mut self) {
        self.pane_focus_requests.clear();
    }

    pub(crate) fn request_pane_focus(&mut self, session: Uuid) {
        self.pane_focus_requests.insert(session);
    }

    /// Drops the zoom of a closed window or of a surface no longer addressable, as upstream's zoom layer clears a
    /// stale target when it renders.
    fn prune_zoom(&mut self) {
        let library = &self.library;
        self.zoom
            .retain(|window, target| library.store(*window).is_some_and(|store| target.is_valid_in(store)));
    }

    pub(crate) fn play_status_sound(&mut self, name: String) {
        self.status_sounds.push(name);
    }

    pub(crate) fn request_window(&mut self, request: WindowRequest) {
        self.window_requests.push(request);
    }

    /// The user brought window `id` to the front (upstream's `WindowAccessor.reportFrontmost`): it becomes the
    /// frontmost window, recorded in the index when that changed, and with auto-hide on, its sidebar shows and the
    /// other windows' hide, on every report. A closed or unknown id is ignored. Commit afterwards.
    pub fn report_frontmost(&mut self, id: Uuid) {
        if !self.library.is_open(id) {
            return;
        }
        self.library.set_frontmost(Some(id));
        if self.auto_hide_inactive_sidebars() {
            self.library.apply_inactive_window_sidebar_hiding();
        }
    }

    /// A control command made window `id` frontmost (upstream's `ControlServer.takeFrontmost`): recorded and, with
    /// auto-hide on, the sidebars reconciled, only when it changed.
    pub(crate) fn take_frontmost(&mut self, id: Uuid) {
        if self.library.frontmost_window_id == Some(id) {
            return;
        }
        self.library.set_frontmost(Some(id));
        if self.auto_hide_inactive_sidebars() {
            self.library.apply_inactive_window_sidebar_hiding();
        }
    }

    fn auto_hide_inactive_sidebars(&self) -> bool {
        self.settings.auto_hide_sidebar_inactive_windows.unwrap_or(false)
    }

    /// Opens closed window `id` from its file (upstream's `loadStore`), or with an empty tree when the model is kept
    /// in memory only. A no-op for an open or unknown id. Commit afterwards.
    pub(crate) fn open_window(&mut self, id: Uuid) {
        match self.persistence.as_mut() {
            Some(persistence) => {
                persistence.load_window(&mut self.library, id);
            }
            None => self.library.open_window(id, AppStore::new()),
        }
    }

    /// Closes window `id`, its snapshot written first (upstream's window `willClose` save, then `closeWindow`); its
    /// metadata stays. The frontend drops its on-screen window once the commit lands. A no-op for a closed or unknown
    /// id and while terminating. Commit afterwards.
    pub fn close_window(&mut self, id: Uuid) {
        match self.persistence.as_mut() {
            Some(persistence) => persistence.close_window(&mut self.library, id),
            None => self.library.close_window(id),
        }
        if !self.library.is_open(id) {
            self.window_states.remove(&id);
        }
    }

    /// Writes open window `id`'s snapshot now (upstream's `saveChecked`); whether it landed. True when the model is
    /// kept in memory only, with nothing to write.
    pub(crate) fn save_window_checked(&mut self, id: Uuid) -> bool {
        match self.persistence.as_mut() {
            Some(persistence) => persistence.save_window(&self.library, id),
            None => true,
        }
    }

    /// Removes window `id` for good, its file deleted; keeps the last window. Commit afterwards.
    pub(crate) fn remove_window(&mut self, id: Uuid) {
        match self.persistence.as_mut() {
            Some(persistence) => persistence.remove_window(&mut self.library, id),
            None => self.library.remove_window(id),
        }
        if !self.library.windows().iter().any(|info| info.id == id) {
            self.window_states.remove(&id);
        }
    }

    /// Replaces the event ring (tests pin its run id and clock). Events already sequenced are dropped with the old
    /// ring.
    #[must_use]
    pub fn with_event_ring(mut self, ring: EventRing) -> Self {
        self.events = ControlEvents::new(ring);
        self
    }

    /// The event ring and its pending `tree.changed` debounces.
    #[must_use]
    pub fn events(&self) -> &ControlEvents {
        &self.events
    }

    /// Records what the frontend shows of window `id`; `None` once it no longer shows it.
    pub fn set_window_state(&mut self, id: Uuid, state: Option<WindowState>) {
        match state {
            Some(state) => self.window_states.insert(id, state),
            None => self.window_states.remove(&id),
        };
    }

    /// What the frontend last reported of window `id`.
    #[must_use]
    pub fn window_state(&self, id: Uuid) -> Option<WindowState> {
        self.window_states.get(&id).copied()
    }

    /// Installs the frontend's reveal of a directory in the file manager, which `session.reveal` calls with the
    /// session's existing focused-pane directory (upstream's `revealSessionInFinder`). Without one the command still
    /// checks the directory and answers.
    pub fn set_reveal_handler(&mut self, handler: Box<dyn Fn(&Path)>) {
        self.reveal_handler = Some(RevealHandler(handler));
    }

    /// Hands `directory` to the reveal handler, if one is installed.
    pub(crate) fn reveal_directory(&self, directory: &Path) {
        if let Some(RevealHandler(handler)) = &self.reveal_handler {
            handler(directory);
        }
    }

    /// The windows and their stores.
    #[must_use]
    pub fn library(&self) -> &WindowLibrary {
        &self.library
    }

    /// The model, for the frontend to put the terminals it creates in their sessions.
    pub fn library_mut(&mut self) -> &mut WindowLibrary {
        &mut self.library
    }

    /// The app settings.
    #[must_use]
    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    /// Lands the model's side effects after a mutation, as every main-loop task that may mutate must: writes the
    /// immediate snapshot saves and a changed index, sequences the events into the ring, and hands back the timers
    /// the caller arms on the main loop. Without persistence the effects are drained and the saves dropped.
    pub fn commit(&mut self) -> CommitTimers {
        self.prune_zoom();
        let (events, debounced, pending_closes) = if let Some(persistence) = self.persistence.as_mut() {
            let commit = persistence.commit(&mut self.library);
            (commit.events, commit.debounced, commit.pending_closes)
        } else {
            let effects = self.library.drain_effects();
            (effects.events, Vec::new(), effects.pending_closes)
        };
        CommitTimers {
            debounced,
            pending_closes,
            tree_changed: self.events.record(events),
        }
    }

    /// A window's `tree.changed` debounce elapsed: sequence the event unless a later change re-armed it.
    pub fn fire_tree_changed(&mut self, timer: TreeChangedTimer) {
        self.events.fire_tree_changed(timer);
    }

    /// A debounced save's quiet window elapsed: write it unless a later save superseded it.
    pub fn fire_debounced(&mut self, save: DebouncedSave) {
        if let Some(persistence) = self.persistence.as_mut() {
            persistence.fire_debounced(&self.library, save);
        }
    }

    /// A soft close's grace ran out: make it final. The caller commits afterwards, as after any mutation.
    pub fn finalize_pending_close(&mut self, timer: PendingCloseTimer) {
        if let Some(store) = self.library.store_mut(timer.window) {
            store.finalize_pending_close(timer.record);
        }
    }

    /// The quit path: makes every pending close final, then writes every open window and the index (upstream's
    /// `applicationWillTerminate` save). Whether every write landed.
    pub fn save_all(&mut self) -> bool {
        self.library.finalize_all_pending_closes();
        self.library.is_terminating = true;
        let _ = self.commit();
        self.persistence
            .as_mut()
            .is_none_or(|persistence| persistence.save_all_checked(&self.library))
    }

    /// Answers one request. Failures become `ok:false` with the error's text.
    ///
    /// A command that waits on the frontend runs its steps at once here (see [`Dispatched::finish_now`]); the app
    /// answers through [`Core::begin`] instead.
    pub fn dispatch(&mut self, request: &Request) -> Response {
        self.begin(request).finish_now(self)
    }

    /// Answers one request, or starts a command that waits on the frontend and hands back its next step.
    pub fn begin(&mut self, request: &Request) -> Dispatched {
        match commands::deferred(self, request) {
            Some(dispatched) => dispatched,
            None => Dispatched::Done(self.route(request).unwrap_or_else(Response::from)),
        }
    }

    /// Decodes and answers one request line, as the socket receives it.
    pub fn dispatch_line(&mut self, line: &[u8]) -> Response {
        match decode_request(line) {
            Ok(request) => self.dispatch(&request),
            Err(error) => error.into(),
        }
    }

    /// The `version` result.
    #[must_use]
    pub fn version(&self) -> VersionResult {
        VersionResult {
            app: self.app.clone(),
            conformance: ConformanceProfile {
                parity_level: PARITY_LEVEL.to_owned(),
                capabilities: self.capabilities.clone(),
            },
        }
    }

    /// The `tree` of the `--window` window, else of the frontmost one; an empty tree once every window is closed.
    ///
    /// # Errors
    ///
    /// The `--window` target does not resolve or names a closed window.
    pub fn tree(&self, window: Option<&str>) -> Result<ControlTree, ControlError> {
        let scoped = Resolver::new(&self.library).placement_window(window)?;
        let empty = AppStore::new();
        let store = scoped.and_then(|id| self.library.store(id)).unwrap_or(&empty);
        let context = TreeContext {
            app: &self.app,
            settings: &self.settings,
            index_unsaved: self.persistence.as_ref().is_some_and(LibraryPersistence::index_unsaved),
            quick_visible: commands::quick::visible(self),
        };
        let mut tree = control_tree(store, &context);
        tree.zoomed_surface = if commands::quick::zoomed(self) {
            Some("quick".to_owned())
        } else {
            scoped.and_then(|id| self.zoom_target(id)).map(SurfaceId::control_id)
        };
        commands::dashboard::project(self, scoped, &mut tree);
        crate::auto_follow::project(self, scoped, &mut tree);
        Ok(tree)
    }

    /// `events.read`: a page of the ring after the cursor, or the anchor alone without one; a cursor the ring cannot
    /// serve fails with the anchor in `result.events` (upstream's `WindowLibrary.readEvents`).
    fn read_events(&self, args: Option<&Args>) -> Result<Response, ControlError> {
        let options = ReadOptions::parse(args)?;
        match self.events.read(&options) {
            Ok(batch) => result_of(&ControlResult::events(batch)),
            Err(failure) => {
                let anchor =
                    serde_json::to_value(ControlResult::events(failure.anchor)).map_err(|_| ControlError::Encode)?;
                Ok(Response::failure_with_result(failure.error.to_string(), anchor))
            }
        }
    }

    /// One arm per group, routing to the group's handler; groups without one yet are refused by name.
    fn route(&mut self, request: &Request) -> Result<Response, ControlError> {
        match request.cmd {
            Command::Session(
                cmd @ (SessionCmd::Status | SessionCmd::Flag | SessionCmd::Context | SessionCmd::Seen),
            ) => match commands::metadata::dispatch(self, cmd, request)? {
                Some(result) => result_of(&result),
                None => Ok(Response::success()),
            },
            Command::Session(SessionCmd::Restart) => result_of(&commands::restart::dispatch(self, request)?),
            Command::Session(cmd @ (SessionCmd::HudOpen | SessionCmd::HudUpdate | SessionCmd::HudClose)) => {
                result_of(&commands::hud::dispatch(self, cmd, request)?)
            }
            Command::Notify => result_of(&commands::notify::dispatch(self, request)?),
            Command::Session(cmd) => result_of(&commands::session::dispatch(self, cmd, request)?),
            Command::Window(cmd) => result_of(&commands::window::dispatch(self, cmd, request)?),
            Command::Workspace(cmd) => match commands::workspace::dispatch(self, cmd, request)? {
                Some(result) => result_of(&result),
                None => Ok(Response::success()),
            },
            Command::Surface(cmd) => match commands::surface_target::dispatch(self, cmd, request)? {
                Some(result) => result_of(&result),
                None => Ok(Response::success()),
            },
            Command::Quick(cmd) => match commands::quick::dispatch(self, cmd, request)? {
                Some(result) => result_of(&result),
                None => Ok(Response::success()),
            },
            Command::Dashboard => match commands::dashboard::dispatch(self, request)? {
                Some(result) => result_of(&result),
                None => Ok(Response::success()),
            },
            Command::Events(EventsCmd::Read) => self.read_events(request.args.as_ref()),
            Command::Version => result_of(&self.version()),
            Command::Tree => {
                let window = request.args.as_ref().and_then(|args| args.window.as_deref());
                result_of(&TreeResult {
                    tree: self.tree(window)?,
                })
            }
            cmd @ (Command::Ask(_)
            | Command::Browser(_)
            | Command::Config(_)
            | Command::Debug(_)
            | Command::Font(_)
            | Command::Hooks(_)
            | Command::Keymap(_)
            | Command::Pick(_)
            | Command::Restore(_)
            | Command::Sidebar(_)
            | Command::Theme(_)
            | Command::Zmx(_)) => Err(ControlError::NotSupported(cmd)),
        }
    }
}

#[cfg(test)]
mod tests {
    use agterm_proto::command::{EventsCmd, SessionCmd, ThemeCmd, WindowCmd, WorkspaceCmd, ZmxCmd};
    use serde_json::json;

    use super::*;

    fn core() -> Core {
        Core::new(
            CoreConfig {
                app: AppIdentity::from_recorded("0.1.0", Some("abc1234")),
                capabilities: vec!["posix-shell".to_owned()],
            },
            CoreState::default(),
        )
    }

    /// The commands answered so far, against a core with no window (the throwaway empty window makes `session.new`
    /// and `session.go` fail on their arguments or their target, as upstream's would, while `workspace.new` creates
    /// its workspace there and answers).
    const ANSWERED: &[Command] = &[
        Command::Version,
        Command::Tree,
        Command::Window(WindowCmd::New),
        Command::Window(WindowCmd::List),
        Command::Events(EventsCmd::Read),
        Command::Workspace(WorkspaceCmd::New),
    ];

    #[test]
    fn version_reports_app_and_profile() {
        let response = core().dispatch(&Request::new(Command::Version));
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({"ok": true, "result": {
                "app": {"version": "0.1.0", "commit": "abc1234"},
                "conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell"]}
            }})
        );
    }

    #[test]
    fn unimplemented_commands_are_refused_by_name() {
        let mut core = core();
        for cmd in [
            Command::Session(SessionCmd::OverlayOpen),
            Command::Theme(ThemeCmd::List),
            Command::Zmx(ZmxCmd::List),
        ] {
            let response = core.dispatch(&Request::new(cmd));
            assert!(!response.ok);
            assert_eq!(
                response.error,
                Some(format!("{} is not supported by this host", cmd.wire_name()))
            );
        }
    }

    fn tree_request(window: Option<&str>) -> Request {
        Request {
            args: window.map(|window| agterm_proto::Args {
                window: Some(window.to_owned()),
                ..agterm_proto::Args::default()
            }),
            ..Request::new(Command::Tree)
        }
    }

    #[test]
    fn tree_projects_the_named_or_frontmost_window() {
        let mut library = WindowLibrary::new("/home/u");
        let first = library.new_window(None);
        let second = library.new_window(None);
        let workspace_of = |window| crate::pane_env::uuid_string(library.store(window).unwrap().workspaces()[0].id());
        let (first_ws, second_ws) = (workspace_of(first), workspace_of(second));
        let mut core = Core::new(
            CoreConfig {
                app: AppIdentity::from_recorded("0.1.0", None),
                capabilities: Vec::new(),
            },
            CoreState {
                library,
                ..CoreState::default()
            },
        );
        let workspace_id = |response: Response| response.result.unwrap()["tree"]["workspaces"][0]["id"].clone();
        assert_eq!(workspace_id(core.dispatch(&tree_request(None))), json!(second_ws));
        let first_prefix = crate::pane_env::uuid_string(first)[..6].to_lowercase();
        assert_eq!(
            workspace_id(core.dispatch(&tree_request(Some(&first_prefix)))),
            json!(first_ws)
        );
        assert_eq!(
            core.dispatch(&tree_request(Some("zzz"))),
            Response::failure("no such window: zzz")
        );
        core.library_mut().close_window(first);
        assert_eq!(
            core.dispatch(&tree_request(Some(&first_prefix))),
            Response::failure("window not open — window.select it first")
        );
        let tree = core.dispatch(&tree_request(None)).result.unwrap();
        assert_eq!(tree["tree"]["app"], json!({"version": "0.1.0"}));
    }

    #[test]
    fn tree_of_no_open_window_is_empty() {
        let response = core().dispatch(&tree_request(None));
        let tree = &response.result.unwrap()["tree"];
        assert_eq!(tree["workspaces"], json!([]));
        assert_eq!(tree["sidebarVisible"], true);
    }

    #[test]
    fn every_command_gets_an_answer() {
        for cmd in Command::all() {
            // a fresh core each, so `window.new` opens no window for the commands after it
            let response = core().dispatch(&Request::new(cmd));
            assert_eq!(response.ok, ANSWERED.contains(&cmd), "{cmd}");
        }
    }

    /// The cases of `test_undecodable_request_is_refused` (`conformance/tests/protocol/test_addressing.py`), with the
    /// full text the suite leaves open.
    #[test]
    fn undecodable_requests_answer_invalid_request_with_the_decoder_context() {
        let mut core = core();
        for (line, error) in [
            (
                &br#"{"cmd":"no.such.command"}"#[..],
                "invalid request: Cannot initialize Command from invalid String value no.such.command",
            ),
            (b"not json", "invalid request: The given data was not valid JSON."),
            (br#"{"target":"active"}"#, "invalid request: missing field `cmd`"),
            (
                b"[]",
                "invalid request: invalid type: sequence, expected a JSON object (Request)",
            ),
            (
                br#"{"cmd":"version","target":5}"#,
                "invalid request: invalid type: integer `5`, expected a string",
            ),
            (
                br#"{"cmd":"events.read","args":{"limit":"5"}}"#,
                r#"invalid request: invalid type: string "5", expected i64"#,
            ),
        ] {
            assert_eq!(core.dispatch_line(line), Response::failure(error), "{line:?}");
        }
    }

    #[test]
    fn other_decode_failures_carry_their_context() {
        let mut core = core();
        for (line, error) in [
            (&b""[..], "invalid request: The given data was not valid JSON."),
            (
                br#"{"cmd":"version"} trailing"#,
                "invalid request: The given data was not valid JSON.",
            ),
            (
                br#"{"cmd":5}"#,
                "invalid request: invalid type: integer `5`, expected a string",
            ),
            (
                br#"{"cmd":"version","args":["x"]}"#,
                "invalid request: invalid type: sequence, expected a JSON object (Args)",
            ),
            (
                br#"{"cmd":"pick.open","args":{"items":[{"id":"1"}]}}"#,
                "invalid request: missing field `label`",
            ),
            // Foundation decodes `cmd` first, so an unknown one is named even next to another defect
            (
                br#"{"args":{"limit":"5"},"cmd":"session.nope","target":5}"#,
                "invalid request: Cannot initialize Command from invalid String value session.nope",
            ),
        ] {
            assert_eq!(core.dispatch_line(line), Response::failure(error), "{line:?}");
        }
        assert!(core.dispatch_line(br#"{"cmd":"version","args":{"bogus":1}}"#).ok);
    }
}
