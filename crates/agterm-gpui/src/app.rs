//! The app shell: who this build is, the core and its control server on the main loop, the windows brought in line
//! with the model after every task (`windows.rs`), the frontend's work landed with the app context, and the quit path.
//! Ported from `agterm-gtk/src/app.rs`.
//!
//! Order on launch follows the GTK build (and upstream's `agtermApp`): the control server settles socket ownership
//! before anything could spawn a shell, so a shell's `AGTERM_SOCKET` is right from the start (`<socket>.unavailable`
//! when another instance owns the socket), and the server starts serving once the state is attached.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use agterm_core::first_run::FirstRunWelcome;
use agterm_core::paths::{self, Env, ProcessEnv};
use agterm_core::persist::{LibraryPersistence, SettingsStore};
use agterm_core::{Core, CoreConfig, CoreState};
use agterm_host::MainLoop;
use agterm_proto::{AppIdentity, Request};
use agterm_server::ControlServer;
use gpui_kit::{App, AppContext, Entity, QuitMode};

use crate::deck::{DeckEnv, Watermarks};
use crate::instance::{Activation, BusRequest};
use crate::main_loop::{self, GpuiMainLoop};
use crate::pane::PaneContext;
use crate::session_host::{self, AppConfig};
use crate::state::{AppState, Poster, post};
use crate::windows::{ConfigHost, Windows};

/// The capabilities of the Rust target profile this build implements (`conformance/docs/rust-build.md`). Each is
/// claimed once its commands work in this frontend (plan > Development Approach), toward the GTK build's set.
/// `posix-shell` waits for the quick terminal: `test_quick_terminal` types into it under that claim.
pub const CAPABILITIES: &[&str] = &[
    "drop-placement",
    "select-takes-frontmost",
    "status-shape",
    "type-rejects-nul",
    "resize-echoes-ratio",
    "hidden-split-holds-focus",
    "modal-blocks-covers",
    "pick-names-pending-ask",
    "theme-slots-by-sync",
    "config-reload-counts",
];

/// The capabilities this host claims: the profile's, and the `live` ones by host
/// ([`session_host::live_capabilities`]).
#[must_use]
pub fn capabilities(live: &[&str]) -> Vec<String> {
    CAPABILITIES.iter().chain(live).copied().map(str::to_owned).collect()
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
    // the launch reap and a pending live sessions reset finish before any window mounts, and the launch spawn order is
    // armed before any window shows (upstream's `GhosttyApp.restoreLaunchDecision` and the pacer's arm at app init)
    session_host::install(core, &ProcessEnv);
    core.arm_launch_spawns(std::time::Instant::now());
    // read at launch: a file edited later takes effect only on reload
    core.load_keymap_at_launch();
    core.load_hooks_at_launch();
    core.load_remotes_at_launch();
}

/// What a primary launch runs with.
pub struct Launch {
    /// The application id, which every window carries as its Wayland app id / X11 class.
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
    let app = gpui_kit::application()
        .with_quit_mode(QuitMode::Explicit)
        .with_assets(crate::titlebar::Icons);
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
    windows: RefCell<Windows>,
    /// Where the core's banners go: the desktop's notification server.
    banners: crate::notifications::Desktop,
    /// The orderly shutdown ran.
    shut_down: Cell<bool>,
}

fn start(cx: &mut App, launch: Launch, main_loop: Arc<GpuiMainLoop<AppState>>, posting: Arc<dyn MainLoop<AppState>>) {
    // before this launch writes anything: the first launch seeds a window and saves it at once
    let had_prior_state = FirstRunWelcome::has_prior_state(&paths::state_dir(&ProcessEnv));
    gpui_kit::init(cx);
    crate::keymap_dispatch::init(cx);
    crate::terminal::register_embedded_fonts(cx.text_system());
    let poster: Poster = Arc::downgrade(&posting);
    let server = ControlServer::new(control_socket(&ProcessEnv));
    let identity = identity();
    let context = PaneContext {
        socket: advertised_socket(&server),
        program_version: identity.version.clone(),
    };
    let resources = session_host::resources();
    let core = launch_core(identity, resources.as_deref(), &poster, context.socket.clone(), cx);
    // the desktop's side before the config loads, so the first terminal starts on it (`appearance.rs`)
    let portal = crate::appearance::Portal::session();
    let dark = portal.as_ref().is_some_and(crate::appearance::Portal::is_dark);
    let windows = windows(context, resources, dark, &poster, &launch);
    let welcome_due = FirstRunWelcome::is_due(core.settings().welcome_shown, had_prior_state);
    let banners = crate::notifications::Desktop::start(launch.app_id.clone(), poster.clone(), cx.background_executor());
    let state = cx.new(|_| {
        let mut state = AppState::new(core);
        state.launched_on(dark);
        state
    });
    let tasks = main_loop.tasks();
    let shell = Rc::new(Shell {
        main_loop,
        posting,
        server: RefCell::new(server),
        state: state.clone(),
        windows: RefCell::new(windows),
        banners,
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
    // last, once the windows are presented: the dialog opens over the frontmost one
    if welcome_due {
        post(&poster, AppState::request_welcome);
    }
    forward_bus_requests(launch.requests, poster.clone(), cx);
    if let Some(portal) = portal
        && let Err(error) = crate::appearance::follow_into(portal, poster.clone())
    {
        tracing::error!(%error, "appearance: the desktop's light/dark changes are not followed");
    }
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

/// The core over the state the last run left, with what it reaches the host through: the theme paths and the settings'
/// ghostty config (written before the config loads, so the first terminal has the theme), the launch hosts, the HUD's
/// painter and the cell a HUD over a session no terminal draws yet is sized with.
fn launch_core(identity: AppIdentity, resources: Option<&Path>, poster: &Poster, socket: String, cx: &App) -> Core {
    let mut core = Core::new(
        CoreConfig {
            app: identity,
            capabilities: capabilities(&session_host::live_capabilities(&ProcessEnv)),
        },
        load_state(&ProcessEnv),
    );
    core.set_theme_paths(
        session_host::themes_dir(resources),
        Some(session_host::settings_config_path(&ProcessEnv)),
    );
    core.write_settings_config();
    install_launch_hosts(&mut core, poster, socket);
    if let Some(helper) = crate::hud::install_helper(&paths::state_dir(&ProcessEnv)) {
        core.set_hud_helper(helper, paths::hud_body_dir(&ProcessEnv));
    }
    core.set_cell_estimator(crate::hud::cell_estimator(Arc::clone(cx.text_system())));
    core
}

/// The windows' registry: the config the terminals draw with (on the `dark` side of the desktop's appearance), the
/// host they launch under, what their shells are told (`context`), Ghostty's `resources`.
fn windows(context: PaneContext, resources: Option<PathBuf>, dark: bool, poster: &Poster, launch: &Launch) -> Windows {
    let config = AppConfig::load(&ProcessEnv, resources.as_deref(), dark);
    let host = Rc::new(RefCell::new(session_host::launch_host(
        &config,
        resources.clone(),
        &ProcessEnv,
    )));
    let overlay_resources = resources.clone();
    let side = Rc::new(Cell::new(dark));
    let overlay_side = Rc::clone(&side);
    let watermarks = Watermarks {
        state_dir: paths::state_dir(&ProcessEnv),
        config_with: Rc::new(move |overlay: &str| {
            AppConfig::load_with(
                &ProcessEnv,
                overlay_resources.as_deref(),
                overlay_side.get(),
                Some(overlay),
            )
            .terminal()
        }),
    };
    let env = Rc::new(DeckEnv::new(
        context,
        session_host::launcher(Rc::clone(&host)),
        config.terminal(),
        poster.clone(),
        watermarks,
    ));
    Windows::new(
        env,
        launch.app_id.clone(),
        Some(ConfigHost {
            resources,
            host,
            dark: side,
        }),
        Some(launch.activation.clone()),
    )
}

/// Brings the windows in line with the model, then lands what the last task left to the frontend.
fn land(shell: &Shell, state: &Entity<AppState>, cx: &mut App) {
    let resettle = state.update(cx, |state, cx| {
        if state.is_quitting() {
            return false;
        }
        shell.windows.borrow_mut().sync(state, cx)
    });
    if resettle {
        // a dropped replacement closed its pane, or a pane the deck focused cleared its badge: committed by the next
        // settle
        let poster: Poster = Arc::downgrade(&shell.posting);
        post(&poster, AppState::settle);
    }
    let work = state.update(cx, |state, _| state.take_frontend_work());
    for activation in &work.activations {
        let frontmost = shell
            .windows
            .borrow()
            .frontmost(&state.read(cx).core)
            .map(|ui| ui.handle);
        if let Some(handle) = frontmost {
            let raised = handle.update(cx, |view, window, cx| {
                crate::activation::activate(window, cx, activation);
                view.present(window, cx);
            });
            if raised.is_err() {
                tracing::debug!("activation: the window is gone");
            }
        }
    }
    land_alerts(shell, state, &work, cx);
    for url in &work.open_urls {
        cx.open_url(url);
    }
    for path in &work.reveals {
        cx.reveal_path(path);
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

/// Lands the status sounds and the banner posts and withdrawals the last tasks queued, and the welcome: the bell and
/// the welcome go to the frontmost window.
fn land_alerts(shell: &Shell, state: &Entity<AppState>, work: &crate::state::FrontendWork, cx: &mut App) {
    if work.sounds.is_empty() && work.banners.is_empty() && !work.welcome {
        return;
    }
    let frontmost = shell
        .windows
        .borrow()
        .frontmost(&state.read(cx).core)
        .map(|ui| ui.handle);
    for name in &work.sounds {
        crate::status_sound::play(name, || {
            if let Some(handle) = frontmost {
                let _ = handle.update(cx, |_, window, cx| crate::status_sound::ring(window, cx));
            }
        });
    }
    crate::notifications::deliver(&shell.banners, &work.banners);
    if work.welcome
        && let Some(handle) = frontmost
        && handle
            .update(cx, |_, window, cx| crate::welcome::present(window, cx))
            .is_err()
    {
        tracing::debug!("welcome: the window is gone");
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
    shell.windows.borrow().teardown_all(cx);
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

#[cfg(test)]
mod tests {
    use agterm_core::paths::MapEnv;

    use super::*;

    #[test]
    fn the_live_capabilities_join_the_profile() {
        assert_eq!(capabilities(&[]), CAPABILITIES);
        let live = capabilities(&[session_host::LIVE_ANY_SHELL]);
        assert_eq!(live.last().map(String::as_str), Some(session_host::LIVE_ANY_SHELL));
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
