//! The core state owner and the command dispatcher: one request in, one response out, on the main thread.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use agterm_proto::{
    AppIdentity, Command, ConformanceProfile, ControlTree, PARITY_LEVEL, Request, Response, VersionResult,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::commands;
use crate::error::ControlError;
use crate::model::PendingCloseTimer;
use crate::model::{AppStore, WindowLibrary};
use crate::persist::{AppSettings, Commit, DebouncedSave, LibraryPersistence};
use crate::projection::{TreeContext, control_tree};
use crate::resolve::Resolver;

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

/// What the frontend reports about one on-screen window, for `window.list` (upstream's window cache flags). A window
/// the frontend has not reported, or no longer shows, has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowState {
    pub fullscreen: bool,
    /// Maximized, not full screen.
    pub zoomed: bool,
    pub minimized: bool,
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
    /// The frontend's on-screen windows, by window id.
    window_states: HashMap<Uuid, WindowState>,
    reveal_handler: Option<RevealHandler>,
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
            window_states: HashMap::new(),
            reveal_handler: None,
        }
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
    /// immediate snapshot saves and a changed index, and hands back what the caller arms on the main loop (the
    /// debounced saves, for [`Core::fire_debounced`], and the soft closes' grace timers, for
    /// [`Core::finalize_pending_close`]) and the events (for the ring, Task 12). Without persistence the effects are
    /// drained and the saves dropped.
    pub fn commit(&mut self) -> Commit {
        if let Some(persistence) = self.persistence.as_mut() {
            return persistence.commit(&mut self.library);
        }
        let effects = self.library.drain_effects();
        Commit {
            events: effects.events,
            debounced: Vec::new(),
            pending_closes: effects.pending_closes,
        }
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
        for id in self.library.open_ids() {
            if let Some(store) = self.library.store_mut(id) {
                store.finalize_all_pending_closes();
            }
        }
        self.library.is_terminating = true;
        let _ = self.commit();
        self.persistence
            .as_mut()
            .is_none_or(|persistence| persistence.save_all_checked(&self.library))
    }

    /// Answers one request. Failures become `ok:false` with the error's text.
    pub fn dispatch(&mut self, request: &Request) -> Response {
        self.route(request).unwrap_or_else(Response::from)
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
            quick_visible: false,
        };
        Ok(control_tree(store, &context))
    }

    /// One arm per group, routing to the group's handler; groups without one yet are refused by name.
    fn route(&mut self, request: &Request) -> Result<Response, ControlError> {
        match request.cmd {
            Command::Session(cmd) => result_of(&commands::session::dispatch(self, cmd, request)?),
            Command::Window(cmd) => result_of(&commands::window::dispatch(self, cmd)?),
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
            | Command::Dashboard
            | Command::Debug(_)
            | Command::Events(_)
            | Command::Font(_)
            | Command::Hooks(_)
            | Command::Keymap(_)
            | Command::Notify
            | Command::Pick(_)
            | Command::Quick(_)
            | Command::Restore(_)
            | Command::Sidebar(_)
            | Command::Surface(_)
            | Command::Theme(_)
            | Command::Workspace(_)
            | Command::Zmx(_)) => Err(ControlError::NotSupported(cmd)),
        }
    }
}

#[cfg(test)]
mod tests {
    use agterm_proto::command::{SessionCmd, WindowCmd, ZmxCmd};
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
    /// and `session.go` fail on their arguments or their target, as upstream's would).
    const ANSWERED: &[Command] = &[Command::Version, Command::Tree, Command::Window(WindowCmd::List)];

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
            Command::Session(SessionCmd::Swap),
            Command::Window(WindowCmd::New),
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
        let mut core = core();
        for cmd in Command::all() {
            let response = core.dispatch(&Request::new(cmd));
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
