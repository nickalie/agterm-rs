//! The command handlers driven through [`Core::dispatch`] over a seeded window library; no surfaces needed. Error
//! strings are asserted in full, dynamic parts included.

mod events;
mod moves;
mod session;
mod window;
mod workspace;

use agterm_proto::command::SessionCmd;
use agterm_proto::{AppIdentity, Args, Command, Request, Response};
use uuid::Uuid;

use crate::model::{AppStore, NewSession, WindowLibrary};
use crate::pane_env::uuid_string;
use crate::persist::AppSettings;
use crate::{Core, CoreConfig, CoreState};

/// A target no upper-case UUID can start with.
const UNKNOWN: &str = "zzz";

/// A core over `library` with `settings`, kept in memory.
fn core_over(library: WindowLibrary, settings: AppSettings) -> Core {
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            persistence: None,
        },
    )
}

/// One open window whose first workspace holds its seeded session plus `extra` more, the last of them selected.
struct Fixture {
    core: Core,
    window: Uuid,
    workspace: Uuid,
    /// The first workspace's sessions in order.
    sessions: Vec<Uuid>,
    /// What [`Fixture::store`] reads once the window is closed.
    empty: AppStore,
}

impl Fixture {
    fn new(extra: usize) -> Self {
        Self::with_settings(extra, AppSettings::default())
    }

    fn with_settings(extra: usize, settings: AppSettings) -> Self {
        let mut library = WindowLibrary::new("/home/u");
        let window = library.new_window(None);
        let mut fixture = Self {
            core: core_over(library, settings),
            window,
            workspace: Uuid::nil(),
            sessions: Vec::new(),
            empty: AppStore::new(),
        };
        fixture.workspace = fixture
            .store()
            .workspaces()
            .first()
            .map_or(Uuid::nil(), crate::model::Workspace::id);
        for _ in 0..extra {
            fixture.add_session(fixture.workspace);
        }
        fixture.sessions = fixture.order(fixture.workspace);
        fixture
    }

    /// The window's store; an empty one once the window is closed.
    fn store(&self) -> &AppStore {
        self.core.library().store(self.window).unwrap_or(&self.empty)
    }

    fn store_mut(&mut self) -> Option<&mut AppStore> {
        self.core.library_mut().store_mut(self.window)
    }

    /// Appends a session at `/tmp` to `workspace`; nil when the workspace is gone.
    fn add_session(&mut self, workspace: Uuid) -> Uuid {
        self.store_mut()
            .and_then(|store| store.add_session(workspace, NewSession::at_cwd("/tmp")))
            .unwrap_or_else(Uuid::nil)
    }

    /// A new workspace with `sessions` sessions.
    fn add_workspace(&mut self, name: &str, sessions: usize) -> (Uuid, Vec<Uuid>) {
        let workspace = self
            .store_mut()
            .map_or_else(Uuid::nil, |store| store.add_workspace(name));
        let ids = (0..sessions).map(|_| self.add_session(workspace)).collect();
        (workspace, ids)
    }

    /// A workspace's session ids in order; empty when it is gone.
    fn order(&self, workspace: Uuid) -> Vec<Uuid> {
        self.store()
            .workspace(workspace)
            .map(|workspace| workspace.sessions.iter().map(crate::model::Session::id).collect())
            .unwrap_or_default()
    }

    fn selected(&self) -> Option<Uuid> {
        self.store().selected_session_id()
    }

    fn send(&mut self, cmd: Command, target: Option<&str>, args: Args) -> Response {
        send(&mut self.core, cmd, target, args)
    }

    fn session(&mut self, cmd: SessionCmd, target: Option<&str>, args: Args) -> Response {
        self.send(Command::Session(cmd), target, args)
    }
}

fn send(core: &mut Core, cmd: Command, target: Option<&str>, args: Args) -> Response {
    core.dispatch(&Request {
        cmd,
        target: target.map(str::to_owned),
        args: Some(args),
    })
}

/// Args with `edit` applied.
fn args(edit: impl FnOnce(&mut Args)) -> Args {
    let mut args = Args::default();
    edit(&mut args);
    args
}

fn wire(id: Uuid) -> String {
    uuid_string(id)
}

/// The `id` a successful response names, parsed back; `None` for anything else.
fn answered_id(response: &Response) -> Option<Uuid> {
    let id = response.result.as_ref()?.get("id")?.as_str()?;
    response.ok.then(|| Uuid::parse_str(id).ok()).flatten()
}

/// The `affected` count of a successful response.
fn answered_affected(response: &Response) -> Option<i64> {
    response
        .ok
        .then(|| response.result.as_ref()?.get("affected")?.as_i64())
        .flatten()
}

fn failure(error: &str) -> Response {
    Response::failure(error)
}
