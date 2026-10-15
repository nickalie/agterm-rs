//! The app shell: who this build is, the core and its control server on the main loop, the frontend's work landed
//! with the app context, and the quit path. Ported from `agterm-gtk/src/app.rs` as a skeleton: one placeholder window
//! stands for the windows Task 18 and Task 26 bring.
//!
//! Order on launch follows the GTK build (and upstream's `agtermApp`): the control server settles socket ownership
//! before anything could spawn a shell, so a shell's `AGTERM_SOCKET` is right from the start (`<socket>.unavailable`
//! when another instance owns the socket), and the server starts serving once the state is attached.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use agterm_core::paths::{self, Env, ProcessEnv};
use agterm_core::persist::{LibraryPersistence, SettingsStore};
use agterm_core::{Core, CoreConfig, CoreState};
use agterm_host::MainLoop;
use agterm_proto::{AppIdentity, Request};
use agterm_server::ControlServer;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement as _, QuitMode, Render,
    Styled, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};

use crate::instance::{Activation, BusRequest};
use crate::main_loop::{self, GpuiMainLoop};
use crate::state::{AppState, Poster, post};

/// The capabilities of the Rust target profile this build implements (`conformance/docs/rust-build.md`): none yet.
/// Each is claimed once its commands work in this frontend (plan > Development Approach), toward the GTK build's set.
pub const CAPABILITIES: &[&str] = &[];

/// The window placeholder's title, until the window shell (Task 18) names windows after their stores.
const PLACEHOLDER_TITLE: &str = "agterm";

/// The capabilities this host claims.
#[must_use]
pub fn capabilities() -> Vec<String> {
    CAPABILITIES.iter().copied().map(str::to_owned).collect()
}

/// The serving build, projected into `version`, `tree.app` and every shell's `TERM_PROGRAM_VERSION`.
#[must_use]
pub fn identity() -> AppIdentity {
    AppIdentity::from_recorded(env!("CARGO_PKG_VERSION"), option_env!("AGTERM_BUILD_COMMIT"))
}

/// The socket the app binds.
#[must_use]
pub fn control_socket(env: &dyn Env) -> PathBuf {
    paths::app_socket_path(env)
}

/// What shells get as `AGTERM_SOCKET`. A path that is not UTF-8 is spelled lossily, which names a path nothing
/// serves: a shell then reaches no instance rather than another one.
#[must_use]
pub fn advertised_socket(server: &ControlServer) -> String {
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

/// The process seam of the hooks and custom commands: children start through [`SystemLauncher`], and each exit comes
/// back as a main-loop task that settles.
///
/// [`SystemLauncher`]: agterm_app::hook_runner::SystemLauncher
fn process_host(poster: &Poster, socket: String) -> agterm_core::ProcessHost {
    let posting = poster.clone();
    agterm_core::ProcessHost {
        launcher: Arc::new(agterm_app::hook_runner::SystemLauncher),
        poster: Arc::new(move |task: agterm_core::CoreTask| {
            post(&posting, move |state: &mut AppState| {
                task(&mut state.core);
                state.settle();
            });
        }),
        socket,
        cli_dir: std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
        path: std::env::var("PATH").ok(),
    }
}

/// What the core reaches the host through, and the launch decisions frozen before the first terminal.
fn install_launch_hosts(core: &mut Core, poster: &Poster, socket: String) {
    core.set_config_locator(paths::ConfigLocator::capture(&ProcessEnv));
    core.set_ui_test_launch(agterm_core::is_ui_test_launch(&ProcessEnv));
    let decision = agterm_app::restore_launch::launch_decision(core.settings(), &ProcessEnv);
    core.set_restore_launch(decision);
    core.load_restore_denylist_at_launch();
    core.set_process_table(
        Box::new(agterm_app::process_table::ProcTable::new()),
        agterm_app::restore_launch::shell_basename(&ProcessEnv),
    );
    core.set_process_host(process_host(poster, socket));
    // read at launch: a file edited later takes effect only on reload
    core.load_keymap_at_launch();
    core.load_hooks_at_launch();
    core.load_remotes_at_launch();
}

/// What a primary launch runs with.
pub struct Launch {
    /// The application id, which the placeholder window carries as its Wayland app id / X11 class.
    pub app_id: String,
    /// The bus requests other launches send (`instance.rs`), fed into the main loop once the app runs.
    pub requests: async_channel::Receiver<BusRequest>,
    /// What this launch was handed (read before GPUI started): its own X11 startup sequence ends on the first window.
    pub activation: Activation,
}

/// Runs the app until it quits; the exit status.
#[must_use]
pub fn run(launch: Launch) -> i32 {
    // GPUI's Linux default quits when the last window closes, which hiding the quick terminal (a removed layer
    // surface) would trigger: this app quits only through its own quit path
    let app = gpui_kit::application().with_quit_mode(QuitMode::Explicit);
    let main_loop = Arc::new(GpuiMainLoop::<AppState>::new(app.background_executor()));
    let posting: Arc<dyn MainLoop<AppState>> = Arc::clone(&main_loop) as _;
    // SIGTERM is this process's from here: it is the primary
    if let Err(error) = crate::signals::quit_on_sigterm(Arc::downgrade(&posting)) {
        tracing::error!(%error, "SIGTERM is not handled: it ends the app without the quit save");
    }
    app.run(move |cx| start(cx, launch, main_loop, posting));
    tracing::info!("quit: the application run returned");
    0
}

/// What the quit path stops, owned by the main thread from launch on.
struct Shell {
    main_loop: Arc<GpuiMainLoop<AppState>>,
    /// The strong side of every [`Poster`]: posters reach the loop while the shell lives.
    posting: Arc<dyn MainLoop<AppState>>,
    server: RefCell<ControlServer>,
    state: Entity<AppState>,
    window: Cell<Option<AnyWindowHandle>>,
    /// The orderly shutdown ran.
    shut_down: Cell<bool>,
}

fn start(cx: &mut App, launch: Launch, main_loop: Arc<GpuiMainLoop<AppState>>, posting: Arc<dyn MainLoop<AppState>>) {
    gpui_kit::init(cx);
    crate::keymap_dispatch::init(cx);
    crate::terminal::register_embedded_fonts(cx.text_system());
    let poster: Poster = Arc::downgrade(&posting);
    let server = ControlServer::new(control_socket(&ProcessEnv));
    let mut core = Core::new(
        CoreConfig {
            app: identity(),
            capabilities: capabilities(),
        },
        load_state(&ProcessEnv),
    );
    install_launch_hosts(&mut core, &poster, advertised_socket(&server));
    let state = cx.new(|_| AppState::new(core));
    let tasks = main_loop.tasks();
    let shell = Rc::new(Shell {
        main_loop,
        posting,
        server: RefCell::new(server),
        state: state.clone(),
        window: Cell::new(None),
        shut_down: Cell::new(false),
    });
    let landing = Rc::clone(&shell);
    main_loop::drive(tasks, state, move |state, cx| land(&landing, state, cx), cx).detach();
    let attaching = poster.clone();
    post(&poster, move |state: &mut AppState| state.attach(attaching));
    let serving = Arc::clone(&shell.posting);
    if let Err(error) = shell
        .server
        .borrow_mut()
        .start(serving, |state: &mut AppState, request: Request| {
            state.handle_request(&request)
        })
    {
        tracing::error!(%error, "the control server is not serving");
    }
    open_placeholder(&shell, &launch.app_id, cx);
    if let Some(window) = shell.window.get()
        && window
            .update(cx, |_, window, cx| {
                crate::activation::launched(window, cx, &launch.activation);
            })
            .is_err()
    {
        tracing::debug!("activation: the first window is gone");
    }
    forward_bus_requests(launch.requests, poster.clone(), cx);
    // the last window closing quits, as in the GTK build
    let closing = poster;
    cx.on_window_closed(move |cx, _| {
        if cx.windows().is_empty() {
            post(&closing, AppState::request_quit);
        }
    })
    .detach();
    // a quit that did not come through the loop still saves and stops the control channel
    let quitting = Rc::clone(&shell);
    cx.on_app_quit(move |cx| {
        shut_down(&quitting, cx);
        async {}
    })
    .detach();
}

/// Lands what the last task left to the frontend, with the state released.
fn land(shell: &Shell, state: &Entity<AppState>, cx: &mut App) {
    let work = state.update(cx, |state, _| state.take_frontend_work());
    for activation in &work.activations {
        if let Some(window) = shell.window.get() {
            let raised = window.update(cx, |_, window, cx| crate::activation::activate(window, cx, activation));
            if raised.is_err() {
                tracing::debug!("activation: the window is gone");
            }
        }
    }
    let poster: Poster = Arc::downgrade(&shell.posting);
    for (action, close_grace) in work.actions {
        crate::keymap_actions::perform(&poster, action, close_grace, cx);
    }
    if work.quit {
        shut_down(shell, cx);
        cx.quit();
    }
}

/// Saves, then stops the main loop and the control channel; once. Requests already queued are answered as shutting
/// down. The quit save runs first, while every terminal still lives (upstream's `applicationWillTerminate`).
fn shut_down(shell: &Shell, cx: &mut App) {
    if shell.shut_down.replace(true) {
        return;
    }
    shell.state.update(cx, |state, _| {
        state.save_on_quit();
        tracing::info!("quit: the state is saved");
        if state.core.arm_live_reset_on_quit() {
            tracing::warn!("quit: the live sessions reset is armed; this build does not relaunch itself yet");
        }
    });
    shell.main_loop.close();
    tracing::info!("quit: the main loop is closed");
    shell.server.borrow_mut().stop();
    tracing::info!("quit: the control server is stopped");
}

/// The bus requests become main-loop tasks, in arrival order.
fn forward_bus_requests(requests: async_channel::Receiver<BusRequest>, poster: Poster, cx: &mut App) {
    cx.spawn(async move |_| {
        while let Ok(request) = requests.recv().await {
            match request {
                BusRequest::Activate(activation) => {
                    post(&poster, move |state: &mut AppState| {
                        state.activation_requested(activation);
                    });
                }
            }
        }
    })
    .detach();
}

/// The one window of the skeleton: the app id names it for the compositor and the window manager.
fn open_placeholder(shell: &Shell, app_id: &str, cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(PLACEHOLDER_TITLE.into()),
            ..TitlebarOptions::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(960.), px(640.)),
            cx,
        ))),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    };
    match gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Placeholder)) {
        Ok((window, _)) => shell.window.set(Some(window)),
        Err(error) => tracing::error!(%error, "the window did not open"),
    }
}

/// An empty view where the window shell will be.
struct Placeholder;

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(div())
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;

    use super::*;

    #[test]
    fn no_capability_is_claimed_before_its_commands_work() {
        assert!(CAPABILITIES.is_empty());
        assert!(capabilities().is_empty());
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
        assert_eq!(advertised_socket(&serving), socket.to_string_lossy());
        assert_eq!(
            advertised_socket(&refused),
            format!("{}.unavailable", socket.to_string_lossy())
        );
        Ok(())
    }

    #[test]
    fn the_identity_is_this_crates_version() {
        assert_eq!(identity().version, env!("CARGO_PKG_VERSION"));
    }
}
