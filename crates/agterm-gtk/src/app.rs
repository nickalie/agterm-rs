//! The app shell: who this build is, the core and its control server on the main loop, the windows, and the quit path.
//!
//! Order on the first activation follows upstream's `agtermApp`: the control server settles socket ownership before
//! any shell is spawned, so a shell's `AGTERM_SOCKET` is right from the start (`<socket>.unavailable` when another
//! instance owns the socket), and the server starts serving once the windows exist. The main loop owns an
//! [`AppState`]: every request is dispatched and then settled there (`state.rs`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use agterm_core::paths::{self, Env, ProcessEnv};
use agterm_core::persist::{LibraryPersistence, SettingsStore};
use agterm_core::{Core, CoreConfig, CoreState};
use agterm_host::MainLoop;
use agterm_proto::{AppIdentity, Request};
use agterm_server::ControlServer;
use gtk::{gio, glib};

use crate::deck::PaneContext;
use crate::ghostty_app;
use crate::main_loop::GlibMainLoop;
use crate::state::AppState;
use crate::surface;
use crate::windows::Windows;

/// The application id the Linux fork registered, kept so this build can take over its state.
const DEFAULT_APP_ID: &str = "io.github.melonamin.agterm";
const APP_ID_ENV: &str = "AGTERM_APP_ID";

/// The capabilities of the Rust target profile implemented so far (`conformance/docs/rust-build.md`).
const CAPABILITIES: &[&str] = &[
    "posix-shell",
    "drop-placement",
    "select-takes-frontmost",
    "status-shape",
    "type-rejects-nul",
    "resize-echoes-ratio",
    "hidden-split-holds-focus",
    "quick-validates-input",
];

/// `AGTERM_APP_ID` when it is a valid application id, else the fork's.
pub fn app_id(env: &dyn Env) -> String {
    env.var(APP_ID_ENV)
        .and_then(|id| id.into_string().ok())
        .filter(|id| gtk::gio::Application::id_is_valid(id))
        .unwrap_or_else(|| DEFAULT_APP_ID.to_owned())
}

/// The serving build, built once per activation and projected into `version`, `tree.app` and every shell's
/// `TERM_PROGRAM_VERSION` (`control-api.md` > Tree and window read-back).
fn identity() -> AppIdentity {
    AppIdentity::from_recorded(env!("CARGO_PKG_VERSION"), option_env!("AGTERM_BUILD_COMMIT"))
}

/// The socket the app binds.
fn control_socket(env: &dyn Env) -> PathBuf {
    paths::app_socket_path(env)
}

/// What shells get as `AGTERM_SOCKET`. A path that is not UTF-8 is spelled lossily, which names a path nothing
/// serves: a shell then reaches no instance rather than another one.
fn advertised_socket(server: &ControlServer) -> String {
    server.advertised_path().to_string_lossy().into_owned()
}

/// The model as the last run left it in the state dir (upstream's launch bootstrap), with the settings.
fn load_state(env: &dyn Env) -> CoreState {
    let state_dir = paths::state_dir(env);
    let home = paths::home_dir(env).map_or_else(|| "/".to_owned(), |home| home.to_string_lossy().into_owned());
    let settings = SettingsStore::new(&state_dir).load();
    let mut persistence = LibraryPersistence::new(&state_dir);
    let library = persistence.bootstrap(&home);
    CoreState {
        library,
        settings,
        persistence: Some(persistence),
    }
}

/// Reveals `path` in the file manager, selected in its folder (upstream's `session.reveal`, which calls
/// `NSWorkspace.activateFileViewerSelecting`), without blocking: the request goes out through the portal or the
/// `FileManager1` D-Bus service and a failure is only logged. The handler for `Core::set_reveal_handler`.
pub fn reveal_in_file_manager(path: &Path) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let parent = gio::Application::default()
        .and_downcast::<gtk::Application>()
        .and_then(|app| app.active_window());
    let shown = path.to_owned();
    launcher.open_containing_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
        if let Err(err) = result {
            tracing::warn!(%err, path = %shown.display(), "could not reveal the directory");
        }
    });
}

/// What the quit path stops, from the first activation on.
struct Shell {
    main_loop: Arc<GlibMainLoop<AppState>>,
    server: ControlServer,
    state: Rc<RefCell<AppState>>,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

pub fn activate(app: &adw::Application) {
    // a later activation (a second launch forwarded here) raises the frontmost window
    let running = SHELL.with(|shell| {
        let shell = shell.borrow();
        if let Some(state) = shell.as_ref().and_then(|shell| shell.state.try_borrow().ok()) {
            state.present();
        }
        shell.is_some()
    });
    if running {
        return;
    }
    let dark = adw::StyleManager::default().is_dark();
    if let Err(err) = ghostty_app::start(dark) {
        tracing::error!(%err, "libghostty did not start");
        app.quit();
        return;
    }
    let identity = identity();
    let mut server = ControlServer::new(control_socket(&ProcessEnv));
    let context = PaneContext {
        socket: advertised_socket(&server),
        program_version: identity.version.clone(),
    };
    let mut core = Core::new(
        CoreConfig {
            app: identity,
            capabilities: CAPABILITIES.iter().map(|name| (*name).to_owned()).collect(),
        },
        load_state(&ProcessEnv),
    );
    core.set_reveal_handler(Box::new(reveal_in_file_manager));
    if let Some(helper) = crate::hud::install_helper(&paths::state_dir(&ProcessEnv)) {
        core.set_hud_helper(helper, paths::hud_body_dir(&ProcessEnv));
    }
    core.set_cell_estimator(Box::new(crate::hud::estimate_cell));
    let state = Rc::new(RefCell::new(AppState::new(core)));
    let main_loop = Arc::new(GlibMainLoop::attach(&glib::MainContext::default(), Rc::clone(&state)));
    let posting: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
    let poster = Arc::downgrade(&posting);
    // not a task: no task runs before the shell exists; every open window renders before it is presented
    crate::notifications::install_actions(app, poster.clone());
    state.borrow_mut().attach(poster, Windows::new(app.clone(), context));
    if let Err(err) = server.start(posting, |state: &mut AppState, request: Request| {
        state.handle_request(&request)
    }) {
        tracing::error!(%err, "the control server is not serving");
    }
    SHELL.with(|shell| {
        *shell.borrow_mut() = Some(Shell {
            main_loop,
            server,
            state,
        });
    });
}

/// Saves, stops the control channel, then tears every terminal down; later calls only repeat the teardown, which then
/// does nothing. Requests already queued are answered as shutting down.
///
/// The quit save runs first, while every terminal still lives (upstream's `applicationWillTerminate`).
pub fn shutdown() {
    if let Some(mut shell) = SHELL.with(|shell| shell.borrow_mut().take()) {
        if let Ok(mut state) = shell.state.try_borrow_mut() {
            state.save_on_quit();
        } else {
            tracing::error!("quit save skipped: the app state is in use");
        }
        shell.main_loop.close();
        shell.server.stop();
    }
    surface::teardown_all();
}

#[cfg(test)]
mod tests {
    use agterm_core::model::WindowLibrary;
    use agterm_core::paths::MapEnv;

    use super::*;
    use crate::deck::PaneSpec;

    #[test]
    fn the_app_id_honours_a_valid_override() {
        assert_eq!(app_id(&MapEnv::new()), DEFAULT_APP_ID);
        let env = MapEnv::new().with(APP_ID_ENV, "io.github.melonamin.agterm.dev.i1");
        assert_eq!(app_id(&env), "io.github.melonamin.agterm.dev.i1");
        for invalid in ["", "agterm", "io..agterm", "io.github.9agterm"] {
            assert_eq!(
                app_id(&MapEnv::new().with(APP_ID_ENV, invalid)),
                DEFAULT_APP_ID,
                "{invalid}"
            );
        }
    }

    #[test]
    fn the_app_binds_the_control_socket_variable_else_the_state_dir() {
        let home = MapEnv::new().with("HOME", "/home/u");
        assert_eq!(
            control_socket(&home),
            PathBuf::from("/home/u/.local/share/agterm/agterm.sock")
        );
        let state = home.clone().with("AGTERM_STATE_DIR", "/tmp/st");
        assert_eq!(control_socket(&state), PathBuf::from("/tmp/st/agterm.sock"));
        let socket = state.with("AGTERM_CONTROL_SOCKET", "/tmp/c.sock");
        assert_eq!(control_socket(&socket), PathBuf::from("/tmp/c.sock"));
        // agtermctl's variable is not the app's
        let pane = home.with("AGTERM_SOCKET", "/tmp/other.sock");
        assert_eq!(
            control_socket(&pane),
            PathBuf::from("/home/u/.local/share/agterm/agterm.sock")
        );
    }

    #[test]
    fn the_terminal_advertises_the_owned_socket_or_the_unavailable_one() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::Builder::new().prefix("agt").tempdir_in("/tmp")?;
        let socket = dir.path().join("agterm.sock");
        let serving = ControlServer::new(&socket);
        let refused = ControlServer::new(&socket);
        let library = WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().ok_or("no window")?;
        let store = library.store(window).ok_or("no store")?;
        let session = store.sessions().next().ok_or("no session")?.id();
        let spec = PaneSpec::primary(window, store, session).ok_or("no page")?;
        let context = |socket| PaneContext {
            socket,
            program_version: identity().version,
        };

        let reachable = spec.env(&context(advertised_socket(&serving)));
        assert_eq!(reachable["AGTERM_SOCKET"], socket.to_string_lossy());
        assert_eq!(reachable["TERM_PROGRAM_VERSION"], env!("CARGO_PKG_VERSION"));
        let unavailable = spec.env(&context(advertised_socket(&refused)));
        assert_eq!(
            unavailable["AGTERM_SOCKET"],
            format!("{}.unavailable", socket.to_string_lossy())
        );
        Ok(())
    }

    #[test]
    fn the_profile_claims_the_capabilities_implemented_so_far() {
        assert_eq!(
            CAPABILITIES,
            [
                "posix-shell",
                "drop-placement",
                "select-takes-frontmost",
                "status-shape",
                "type-rejects-nul",
                "resize-echoes-ratio",
                "hidden-split-holds-focus",
                "quick-validates-input"
            ]
        );
    }
}
