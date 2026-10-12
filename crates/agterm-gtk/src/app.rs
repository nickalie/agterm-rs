//! The app shell: who this build is, the core and its control server on the main loop, the window, and the quit path.
//!
//! Order on the first activation follows upstream's `agtermApp`: the control server settles socket ownership before
//! any shell is spawned, so a shell's `AGTERM_SOCKET` is right from the start (`<socket>.unavailable` when another
//! instance owns the socket), and the server starts serving once the window exists.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use agterm_core::pane_env::{self, PaneEnv, SessionPane, StatusPane};
use agterm_core::paths::{self, Env, ProcessEnv};
use agterm_core::{Core, CoreConfig};
use agterm_host::MainLoop;
use agterm_proto::{AppIdentity, Request};
use agterm_server::{ControlServer, Reply};
use gtk::glib;
use uuid::Uuid;

use crate::ghostty_app;
use crate::main_loop::GlibMainLoop;
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

/// The single terminal's environment: the main pane of a session. Interim until the model (Task 11) gives it real
/// session, window and workspace ids.
fn primary_pane_env(identity: &AppIdentity, socket: &str) -> PaneEnv {
    let token = pane_env::uuid_string(Uuid::new_v4());
    pane_env::session(&SessionPane {
        session: Uuid::new_v4(),
        window: None,
        workspace: None,
        socket_path: socket,
        program_version: &identity.version,
        pane: Some(StatusPane::Left),
        pane_token: Some(&token),
    })
}

/// What the quit path stops, from the first activation on.
struct Shell {
    main_loop: Arc<GlibMainLoop<Core>>,
    server: ControlServer,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn build_window(app: &adw::Application, env: PaneEnv) {
    let terminal = TerminalSurface::new(SurfaceSpec {
        env: env.into_iter().collect(),
        ..SurfaceSpec::default()
    });
    let header = adw::HeaderBar::new();
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(terminal.widget()));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("agterm")
        .default_width(960)
        .default_height(640)
        .content(&content)
        .build();
    let titled = window.downgrade();
    terminal.connect_title(move |title| {
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
    build_window(app, primary_pane_env(&identity, &advertised_socket(&server)));
    let core = Core::new(CoreConfig {
        app: identity,
        capabilities: CAPABILITIES.iter().map(|name| (*name).to_owned()).collect(),
    });
    let main_loop = Arc::new(GlibMainLoop::attach(
        &glib::MainContext::default(),
        Rc::new(RefCell::new(core)),
    ));
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

        let reachable = primary_pane_env(&identity, &advertised_socket(&serving));
        assert_eq!(reachable["AGTERM_SOCKET"], socket.to_string_lossy());
        let unavailable = primary_pane_env(&identity, &advertised_socket(&refused));
        assert_eq!(
            unavailable["AGTERM_SOCKET"],
            format!("{}.unavailable", socket.to_string_lossy())
        );
        Ok(())
    }

    #[test]
    fn the_terminal_is_the_main_pane_of_a_session_on_this_build() {
        let env = primary_pane_env(&identity(), "/tmp/a.sock");
        assert_eq!(env["TERM_PROGRAM"], "agterm");
        assert_eq!(env["TERM_PROGRAM_VERSION"], env!("CARGO_PKG_VERSION"));
        assert_eq!(env["AGTERM_PANE"], "left");
        for id in ["AGTERM_SESSION_ID", "AGTERM_PANE_ID"] {
            assert!(Uuid::parse_str(&env[id]).is_ok(), "{id}: {}", env[id]);
            assert_eq!(env[id], env[id].to_uppercase(), "{id}");
        }
        assert!(!env.contains_key("AGTERM_WINDOW_ID"));
    }
}
