//! The app shell: who this build is, the core and its control server on the main loop, the window, and the quit path.
//!
//! Order on the first activation follows upstream's `agtermApp`: the control server settles socket ownership before
//! any shell is spawned, so a shell's `AGTERM_SOCKET` is right from the start (`<socket>.unavailable` when another
//! instance owns the socket), and the server starts serving once the window exists.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use adw::prelude::*;
use agterm_core::model::WindowLibrary;
use agterm_core::pane_env::{self, PaneEnv, SessionPane, StatusPane};
use agterm_core::paths::{self, Env, ProcessEnv};
use agterm_core::persist::{LibraryPersistence, SettingsStore};
use agterm_core::{Core, CoreConfig, CoreState};
use agterm_host::MainLoop;
use agterm_proto::{AppIdentity, Request};
use agterm_server::{ControlServer, Reply};
use gtk::glib;
use uuid::Uuid;

use crate::ghostty_app;
use crate::main_loop::GlibMainLoop;
use crate::pane::PaneSurface;
use crate::surface::{self, SurfaceSpec, TerminalSurface};

/// The application id the Linux fork registered, kept so this build can take over its state.
const DEFAULT_APP_ID: &str = "io.github.melonamin.agterm";
const APP_ID_ENV: &str = "AGTERM_APP_ID";

/// The capabilities of the Rust target profile implemented so far (`conformance/docs/rust-build.md`).
const CAPABILITIES: &[&str] = &["posix-shell"];

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

/// The session pane the window shows. Interim until the eager deck (Task 11) gives every session its terminal: the
/// frontmost window's selected session, else its first, gets the one terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShownPane {
    window: Uuid,
    workspace: Uuid,
    session: Uuid,
    pane_identity: Uuid,
    cwd: String,
}

fn shown_pane(library: &WindowLibrary) -> Option<ShownPane> {
    let window = library.active_window_id()?;
    let store = library.store(window)?;
    let session = store.active_session().or_else(|| store.sessions().next())?;
    Some(ShownPane {
        window,
        workspace: store.workspace_for_session(session.id())?.id(),
        session: session.id(),
        pane_identity: session.pane_identity,
        cwd: session.effective_cwd().to_owned(),
    })
}

/// The main pane's environment, its pane token the pane identity (upstream's `paneIdentity.uuidString`).
fn primary_pane_env(identity: &AppIdentity, socket: &str, pane: &ShownPane) -> PaneEnv {
    let token = pane_env::uuid_string(pane.pane_identity);
    pane_env::session(&SessionPane {
        session: pane.session,
        window: Some(pane.window),
        workspace: Some(pane.workspace),
        socket_path: socket,
        program_version: &identity.version,
        pane: Some(StatusPane::Left),
        pane_token: Some(&token),
    })
}

/// Creates the shown pane's terminal and puts it in its session's slot, where `tree` reads it back.
fn create_terminal(core: &mut Core, pane: &ShownPane, env: PaneEnv) -> TerminalSurface {
    let terminal = TerminalSurface::new(SurfaceSpec {
        working_directory: Some(PathBuf::from(&pane.cwd)),
        env: env.into_iter().collect(),
        ..SurfaceSpec::default()
    });
    let token = pane_env::uuid_string(pane.pane_identity);
    if let Some(session) = core
        .library_mut()
        .store_mut(pane.window)
        .and_then(|store| store.session_mut(pane.session))
    {
        session
            .surface
            .replace(Box::new(PaneSurface::new(terminal.clone(), token)));
    }
    terminal
}

/// A terminal report bound for the session that shows the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneReport {
    /// An OSC 7 working directory.
    Pwd(String),
    /// A title a program set (or the user's static one).
    Title(String),
}

/// Lands a primary pane's report on its session, wherever that session lives now; a session gone since is skipped.
fn apply_report(core: &mut Core, session: Uuid, report: &PaneReport) {
    let Some(session) = core
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
}

/// Forwards the shown terminal's cwd and title reports to its session, where `tree` reads them back (upstream's
/// `applyPwd`/`applyTitle`). Each report is posted rather than applied in place: libghostty may call back while the
/// core is borrowed. Nothing is saved, as upstream.
fn forward_reports(terminal: &TerminalSurface, main_loop: &Arc<GlibMainLoop<Core>>, session: Uuid) {
    let post = move |main_loop: &Weak<GlibMainLoop<Core>>, report: PaneReport| {
        let Some(main_loop) = main_loop.upgrade() else {
            return;
        };
        let task = Box::new(move |core: &mut Core| apply_report(core, session, &report));
        if main_loop.post(task).is_err() {
            tracing::debug!("terminal report dropped: the main loop is closed");
        }
    };
    let posting = Arc::downgrade(main_loop);
    terminal.connect_pwd(move |pwd| post(&posting, PaneReport::Pwd(pwd.to_owned())));
    let posting = Arc::downgrade(main_loop);
    terminal.connect_title(move |title| post(&posting, PaneReport::Title(title.to_owned())));
}

/// What the quit path stops, from the first activation on.
struct Shell {
    main_loop: Arc<GlibMainLoop<Core>>,
    server: ControlServer,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn build_window(app: &adw::Application, terminal: Option<&TerminalSurface>) {
    let header = adw::HeaderBar::new();
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("agterm")
        .default_width(960)
        .default_height(640)
        .content(&content)
        .build();
    let Some(terminal) = terminal else {
        window.present();
        return;
    };
    content.set_content(Some(terminal.widget()));
    let titled = window.downgrade();
    terminal.connect_title(move |title| {
        if title.is_empty() {
            return;
        }
        if let Some(window) = titled.upgrade() {
            window.set_title(Some(title));
        }
    });
    let closing = window.downgrade();
    terminal.connect_exit(move || {
        if let Some(window) = closing.upgrade() {
            window.close();
        }
    });
    window.present();
    terminal.grab_focus();
}

pub fn activate(app: &adw::Application) {
    if SHELL.with(|shell| shell.borrow().is_some()) {
        if let Some(window) = app.active_window() {
            window.present();
        }
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
    let socket = advertised_socket(&server);
    let mut core = Core::new(
        CoreConfig {
            app: identity.clone(),
            capabilities: CAPABILITIES.iter().map(|name| (*name).to_owned()).collect(),
        },
        load_state(&ProcessEnv),
    );
    let shown = shown_pane(core.library());
    let terminal = shown.as_ref().map(|pane| {
        let env = primary_pane_env(&identity, &socket, pane);
        create_terminal(&mut core, pane, env)
    });
    build_window(app, terminal.as_ref());
    let main_loop = Arc::new(GlibMainLoop::attach(
        &glib::MainContext::default(),
        Rc::new(RefCell::new(core)),
    ));
    if let (Some(terminal), Some(pane)) = (&terminal, &shown) {
        forward_reports(terminal, &main_loop, pane.session);
    }
    let posting: Arc<dyn MainLoop<Core>> = Arc::clone(&main_loop) as _;
    let handle = |core: &mut Core, request: Request| -> Reply { core.dispatch(&request).into() };
    if let Err(err) = server.start(posting, handle) {
        tracing::error!(%err, "the control server is not serving");
    }
    SHELL.with(|shell| *shell.borrow_mut() = Some(Shell { main_loop, server }));
}

/// Stops the control channel, then tears every terminal down; later calls only repeat the teardown, which then does
/// nothing. Requests already queued are answered as shutting down.
pub fn shutdown() {
    if let Some(mut shell) = SHELL.with(|shell| shell.borrow_mut().take()) {
        shell.main_loop.close();
        shell.server.stop();
    }
    surface::teardown_all();
}

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;

    use super::*;

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
        let identity = AppIdentity::from_recorded("1.2.3", None);
        let pane = shown_pane(&WindowLibrary::seeded("/home/u")).ok_or("no pane")?;

        let reachable = primary_pane_env(&identity, &advertised_socket(&serving), &pane);
        assert_eq!(reachable["AGTERM_SOCKET"], socket.to_string_lossy());
        let unavailable = primary_pane_env(&identity, &advertised_socket(&refused), &pane);
        assert_eq!(
            unavailable["AGTERM_SOCKET"],
            format!("{}.unavailable", socket.to_string_lossy())
        );
        Ok(())
    }

    #[test]
    fn the_terminal_is_the_main_pane_of_the_shown_session_on_this_build() {
        let library = WindowLibrary::seeded("/home/u");
        let pane = shown_pane(&library).unwrap();
        let store = library.active_store().unwrap();
        let session = store.active_session().unwrap();
        assert_eq!(pane.session, session.id());
        assert_eq!(pane.cwd, "/home/u");
        let env = primary_pane_env(&identity(), "/tmp/a.sock", &pane);
        assert_eq!(env["TERM_PROGRAM"], "agterm");
        assert_eq!(env["TERM_PROGRAM_VERSION"], env!("CARGO_PKG_VERSION"));
        assert_eq!(env["AGTERM_PANE"], "left");
        assert_eq!(env["AGTERM_SESSION_ID"], pane_env::uuid_string(session.id()));
        assert_eq!(env["AGTERM_PANE_ID"], pane_env::uuid_string(session.pane_identity));
        assert_eq!(env["AGTERM_WINDOW_ID"], pane_env::uuid_string(pane.window));
        assert_eq!(env["AGTERM_WORKSPACE_ID"], pane_env::uuid_string(pane.workspace));
    }

    #[test]
    fn a_window_without_sessions_shows_no_terminal() {
        let mut library = WindowLibrary::new("/home/u");
        library.new_window(None);
        let window = library.active_window_id().unwrap();
        let store = library.store_mut(window).unwrap();
        let ids: Vec<Uuid> = store.sessions().map(agterm_core::model::Session::id).collect();
        for id in ids {
            store.close_session(id);
        }
        assert_eq!(shown_pane(&library), None);
    }

    #[test]
    fn a_terminal_report_lands_on_its_session_in_any_window() {
        let mut library = WindowLibrary::seeded("/home/u");
        let session = shown_pane(&library).unwrap().session;
        library.new_window(None);
        let mut core = Core::new(
            CoreConfig {
                app: identity(),
                capabilities: Vec::new(),
            },
            CoreState {
                library,
                ..CoreState::default()
            },
        );
        apply_report(&mut core, session, &PaneReport::Pwd("/tmp/next".to_owned()));
        apply_report(&mut core, session, &PaneReport::Title("vim".to_owned()));
        apply_report(&mut core, Uuid::new_v4(), &PaneReport::Title("lost".to_owned()));
        let reported = core
            .library()
            .store_for_session(session)
            .unwrap()
            .session(session)
            .unwrap();
        assert_eq!(reported.effective_cwd(), "/tmp/next");
        assert_eq!(reported.osc_title.as_deref(), Some("vim"));
        assert_eq!(reported.display_name(), "vim");
    }

    #[test]
    fn the_shown_pane_falls_back_to_the_first_session() {
        let mut library = WindowLibrary::seeded("/home/u");
        let store = library.active_store_mut().unwrap();
        let first = store.sessions().next().unwrap().id();
        store.select_session(None);
        assert_eq!(shown_pane(&library).unwrap().session, first);
    }
}
