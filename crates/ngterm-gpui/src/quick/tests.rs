//! The quick terminal panel: its host and frame decisions (the layer surface's attributes and placement on integer and
//! fractional-scale displays, the window host's), and the panel in GPUI's test platform over a real core and a real
//! pane, on the window host (the test platform is no Wayland): shown, focused, hidden with its shell kept, shown
//! again in a new window, dismissed by a focus loss and by the window manager's close, and gone with its shell.
//!
//! The pane runs `/bin/sh -c 'exec cat'` with no `NGTERM_*`, `AGTERM_*` or `ZMX_*` variable inherited (CLAUDE.md >
//! Never touch the user's running ngterm).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::{Bounds, TestAppContext, VisualTestContext, WindowBounds, WindowKind, point, px, size};
use ngterm_core::{CoreConfig, CoreState};
use ngterm_host::{MainLoop, MainLoopClosed, MainTask};
use ngterm_proto::AppIdentity;
use ngterm_term::{Launch, Pane, PaneCommand, PaneConfig};

use super::*;
use crate::deck::Watermarks;
use crate::terminal::{Spawn, TerminalConfig};

const DEADLINE: Duration = Duration::from_secs(20);

fn one() -> DisplayId {
    DisplayId::new(1)
}

fn two() -> DisplayId {
    DisplayId::new(2)
}

fn fit(zoomed: bool) -> Fit {
    Fit {
        size_percent: None,
        zoomed,
    }
}

fn frame(host: QuickHost, zoomed: bool, sized_for: Option<DisplayId>, landed: Option<DisplayId>) -> Frame {
    Frame {
        host,
        fit: fit(zoomed),
        sized_for,
        landed,
        named: false,
    }
}

fn display(id: DisplayId, width: f32, height: f32) -> Target {
    Target {
        id,
        bounds: Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(width), px(height)),
        },
    }
}

// MARK: - decisions

#[test]
fn the_host_is_a_layer_surface_on_wayland_and_a_window_elsewhere() {
    assert_eq!(first_host("Wayland"), QuickHost::LayerShell);
    assert_eq!(
        first_host("X11"),
        QuickHost::Window,
        "GPUI opens a layer surface on X11 as a plain window"
    );
    assert_eq!(first_host("headless"), QuickHost::Window);
    assert_eq!(first_host(""), QuickHost::Window);
}

#[test]
fn only_a_compositor_without_layer_shell_moves_the_panel_to_a_window() {
    let refused: gpui_kit::Result<()> = Err(gpui_kit::layer_shell::LayerShellNotSupportedError.into());
    assert!(refuses_layer_shell(&refused));
    let failed: gpui_kit::Result<()> = Err(std::io::Error::other("no GPU").into());
    assert!(!refuses_layer_shell(&failed));
    assert!(!refuses_layer_shell(&Ok(())));
}

#[test]
fn a_panel_not_on_screen_opens() {
    assert_eq!(step(None, fit(false)), Step::Open);
    assert_eq!(step(None, fit(true)), Step::Open);
}

#[test]
fn a_shown_panel_framed_as_asked_stays() {
    let shown = frame(QuickHost::LayerShell, false, Some(one()), Some(one()));
    assert_eq!(step(Some(&shown), fit(false)), Step::Keep);
    let window = frame(QuickHost::Window, false, Some(one()), Some(two()));
    assert_eq!(
        step(Some(&window), fit(false)),
        Step::Keep,
        "the window host is never refitted"
    );
    let percent = Fit {
        size_percent: Some(50),
        zoomed: false,
    };
    assert_eq!(
        step(Some(&shown), percent),
        Step::Keep,
        "a size setting waits for the next show"
    );
}

#[test]
fn the_window_host_zooms_in_place() {
    let window = frame(QuickHost::Window, false, Some(one()), Some(one()));
    assert_eq!(step(Some(&window), fit(true)), Step::Zoom(true));
    let zoomed = frame(QuickHost::Window, true, Some(one()), Some(one()));
    assert_eq!(step(Some(&zoomed), fit(false)), Step::Zoom(false));
}

#[test]
fn the_layer_surface_reopens_for_a_zoom_on_the_display_it_landed_on() {
    let shown = frame(QuickHost::LayerShell, false, Some(one()), Some(two()));
    assert_eq!(step(Some(&shown), fit(true)), Step::Reopen(Some(two())));
    let unreported = frame(QuickHost::LayerShell, true, Some(one()), None);
    assert_eq!(step(Some(&unreported), fit(false)), Step::Reopen(Some(one())));
    let nowhere = frame(QuickHost::LayerShell, false, None, None);
    assert_eq!(step(Some(&nowhere), fit(true)), Step::Reopen(None));
}

#[test]
fn a_layer_surface_landing_off_its_estimate_is_refitted_once() {
    let off = frame(QuickHost::LayerShell, false, Some(one()), Some(two()));
    assert_eq!(step(Some(&off), fit(false)), Step::Reopen(Some(two())));
    let estimated_nowhere = frame(QuickHost::LayerShell, false, None, Some(two()));
    assert_eq!(step(Some(&estimated_nowhere), fit(false)), Step::Reopen(Some(two())));
    let refitted = Frame { named: true, ..off };
    assert_eq!(
        step(Some(&refitted), fit(false)),
        Step::Keep,
        "a named surface stays where it was put"
    );
    let zoomed = frame(QuickHost::LayerShell, true, Some(one()), Some(two()));
    assert_eq!(
        step(Some(&zoomed), fit(true)),
        Step::Keep,
        "a zoomed panel fills whatever output it is on"
    );
    let unreported = frame(QuickHost::LayerShell, false, Some(one()), None);
    assert_eq!(step(Some(&unreported), fit(false)), Step::Keep);
}

/// The layer surface's options, split out.
fn layer(options: &gpui_kit::WindowOptions) -> &gpui_kit::layer_shell::LayerShellOptions {
    let WindowKind::LayerShell(layer) = &options.kind else {
        panic!("not a layer surface: {:?}", options.kind);
    };
    layer
}

fn windowed_size(options: &gpui_kit::WindowOptions) -> gpui_kit::Size<gpui_kit::Pixels> {
    match options.window_bounds {
        Some(WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds)) => bounds.size,
        other => panic!("no bounds: {other:?}"),
    }
}

#[test]
fn the_layer_surface_takes_the_gtk_builds_attributes_centered_on_its_display() {
    use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer};
    let monitor = display(one(), 2560., 1440.);
    let options = window_options(QuickHost::LayerShell, fit(false), Some(&monitor), false, "io.test").unwrap();
    let surface = layer(&options);
    assert_eq!(surface.namespace, NAMESPACE);
    assert_eq!(surface.layer, Layer::Overlay);
    assert_eq!(surface.keyboard_interactivity, KeyboardInteractivity::OnDemand);
    assert_eq!(surface.exclusive_zone, Some(px(0.)));
    assert_eq!(surface.anchor, Anchor::TOP);
    assert_eq!(surface.margin, Some((px(370.), px(0.), px(0.), px(0.))));
    assert_eq!(windowed_size(&options), size(px(1100.), px(700.)));
    assert_eq!(options.display_id, None, "no output named as it comes on screen");
    assert_eq!(options.app_id.as_deref(), Some("io.test"));
    assert_eq!(
        options.window_background,
        gpui_kit::WindowBackgroundAppearance::Transparent
    );

    let named = window_options(QuickHost::LayerShell, fit(false), Some(&monitor), true, "io.test").unwrap();
    assert_eq!(named.display_id, Some(one()), "a refit names the output it landed on");
}

#[test]
fn the_zoomed_layer_surface_fills_its_output() {
    use gpui_kit::layer_shell::Anchor;
    for monitor in [Some(display(one(), 1920., 1080.)), None] {
        let options = window_options(QuickHost::LayerShell, fit(true), monitor.as_ref(), true, "io.test").unwrap();
        let surface = layer(&options);
        assert_eq!(
            surface.anchor,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
        );
        assert_eq!(surface.margin, None);
        assert_eq!(windowed_size(&options), size(px(0.), px(0.)));
    }
}

#[test]
fn a_centered_layer_surface_needs_a_display() {
    assert!(window_options(QuickHost::LayerShell, fit(false), None, false, "io.test").is_none());
}

#[test]
fn the_placement_follows_a_fractional_outputs_logical_size() {
    // eDP-1 at scale 1.75 (2880x1800): its logical size, as xdg-output reports it, and what the integer scale gave
    let logical = display(one(), 1645., 1028.);
    let integer = display(one(), 1440., 900.);
    let margin = |monitor: &Target| {
        let options = window_options(QuickHost::LayerShell, fit(false), Some(monitor), false, "io.test").unwrap();
        (layer(&options).margin, windowed_size(&options))
    };
    assert_eq!(
        margin(&logical),
        (Some((px(164.), px(0.), px(0.), px(0.))), size(px(1100.), px(700.)))
    );
    assert_eq!(
        margin(&integer),
        (Some((px(100.), px(0.), px(0.), px(0.))), size(px(1100.), px(700.))),
        "64 px higher on the integer-scale bounds"
    );
    let percent = Fit {
        size_percent: Some(50),
        zoomed: false,
    };
    let options = window_options(QuickHost::LayerShell, percent, Some(&logical), false, "io.test").unwrap();
    assert_eq!(windowed_size(&options), size(px(823.), px(514.)));
    assert_eq!(layer(&options).margin, Some((px(257.), px(0.), px(0.), px(0.))));
}

#[test]
fn the_window_host_is_undecorated_centered_and_maximized_for_the_zoom() {
    let monitor = Target {
        id: two(),
        bounds: Bounds {
            origin: point(px(1280.), px(0.)),
            size: size(px(1280.), px(800.)),
        },
    };
    let options = window_options(QuickHost::Window, fit(false), Some(&monitor), false, "io.test").unwrap();
    assert_eq!(options.kind, WindowKind::Normal);
    assert_eq!(options.window_decorations, Some(gpui_kit::WindowDecorations::Client));
    assert_eq!(
        options.window_background,
        gpui_kit::WindowBackgroundAppearance::Transparent
    );
    assert_eq!(
        options
            .titlebar
            .as_ref()
            .and_then(|bar| bar.title.as_ref())
            .map(AsRef::as_ref),
        Some(TITLE)
    );
    assert!(!options.is_resizable && !options.is_movable);
    assert_eq!(
        options.window_bounds,
        Some(WindowBounds::Windowed(Bounds {
            origin: point(px(1370.), px(50.)),
            size: size(px(1100.), px(700.)),
        }))
    );
    assert_eq!(options.display_id, Some(two()));
    let zoomed = window_options(QuickHost::Window, fit(true), Some(&monitor), false, "io.test").unwrap();
    assert!(matches!(zoomed.window_bounds, Some(WindowBounds::Maximized(_))));
    let nowhere = window_options(QuickHost::Window, fit(false), None, false, "io.test").unwrap();
    assert_eq!(windowed_size(&nowhere), size(px(1100.), px(700.)));
}

#[test]
fn the_quick_terminal_runs_a_login_shell_with_the_quick_environment() {
    let context = PaneContext {
        socket: "/run/ngterm.sock".to_owned(),
        program_version: "0.36.0".to_owned(),
    };
    let launch = launch("/home/u".to_owned(), &context);
    assert_eq!(launch.working_directory, Some(PathBuf::from("/home/u")));
    assert_eq!(launch.command, None);
    assert!(!launch.wait_after_command);
    let expected: Vec<(String, String)> = pane_env::quick_terminal("/run/ngterm.sock", "0.36.0")
        .into_iter()
        .collect();
    assert_eq!(launch.env, expected);
    for name in [
        "NGTERM_SESSION_ID",
        "NGTERM_WINDOW_ID",
        "NGTERM_WORKSPACE_ID",
        "NGTERM_PANE_ID",
    ] {
        assert!(launch.env.iter().all(|(key, _)| key != name), "{name}");
    }
}

// MARK: - the panel in GPUI's test platform

/// A loop that keeps what is posted for the test to run.
#[derive(Default)]
struct Posted(Mutex<Vec<MainTask<AppState>>>);

impl MainLoop<AppState> for Posted {
    fn post(&self, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.0.lock().unwrap().push(task);
        Ok(())
    }

    fn post_after(&self, _: Duration, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.post(task)
    }
}

/// `/bin/sh -c <script>` with the inherited `NGTERM_*`/`AGTERM_*`/`ZMX_*` variables removed, whatever the launch asked.
fn quiet_launcher(launches: Arc<Mutex<Vec<Launch>>>, script: &'static str) -> crate::pane::Launcher {
    Rc::new(move |launch: &Launch| -> Spawn {
        launches.lock().unwrap().push(launch.clone());
        let mut command = PaneCommand::new("/bin/sh").arg("-c").arg(script);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("NGTERM_") || name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
                command = command.env_remove(key);
            }
        }
        Box::new(move |config: PaneConfig| Pane::spawn(&command, &config))
    })
}

struct Harness<'a> {
    state: AppState,
    posted: Arc<Posted>,
    _loop: Arc<dyn MainLoop<AppState>>,
    env: DeckEnv,
    panel: QuickPanel,
    launches: Arc<Mutex<Vec<Launch>>>,
    _state_dir: tempfile::TempDir,
    cx: &'a mut TestAppContext,
}

fn harness(cx: &mut TestAppContext) -> Harness<'_> {
    harness_running(cx, "exec cat")
}

/// The harness with the quick terminal running `/bin/sh -c <script>`.
fn harness_running<'a>(cx: &'a mut TestAppContext, script: &'static str) -> Harness<'a> {
    // the pane's thread wakes its pump from outside the test scheduler
    cx.executor().allow_parking();
    let (library, _, _) = crate::pane::tests::three_sessions();
    let core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            ..CoreState::default()
        },
    );
    let posted = Arc::new(Posted::default());
    let main_loop: Arc<dyn MainLoop<AppState>> = Arc::clone(&posted) as _;
    let mut state = AppState::new(core);
    state.attach(Arc::downgrade(&main_loop));
    let launches = Arc::new(Mutex::new(Vec::new()));
    let state_dir = tempfile::tempdir().unwrap();
    let env = DeckEnv::new(
        PaneContext {
            socket: "/tmp/ngterm-test.sock".to_owned(),
            program_version: "1.2.3".to_owned(),
        },
        quiet_launcher(Arc::clone(&launches), script),
        TerminalConfig::default(),
        Arc::downgrade(&main_loop),
        Watermarks {
            state_dir: state_dir.path().to_path_buf(),
            config_with: Rc::new(|_: &str| TerminalConfig::default()),
        },
    );
    Harness {
        state,
        posted,
        _loop: main_loop,
        env,
        panel: QuickPanel::default(),
        launches,
        _state_dir: state_dir,
        cx,
    }
}

impl Harness<'_> {
    /// Runs the posted tasks, then the panel's sync: one turn of the app's loop.
    fn turn(&mut self) {
        loop {
            let tasks = std::mem::take(&mut *self.posted.0.lock().unwrap());
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                task(&mut self.state);
            }
        }
        let (panel, state, env) = (&mut self.panel, &mut self.state, &self.env);
        self.cx.update(|cx| {
            let quick = QuickEnv {
                env,
                app_id: "com.nickalie.ngterm.test",
                frontmost: None,
            };
            panel.sync(&mut state.core, &quick, cx);
        });
        self.cx.run_until_parked();
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            self.turn();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn window(&self) -> WindowHandle<QuickView> {
        self.panel.window().expect("the panel is on screen")
    }

    fn visual(&mut self) -> VisualTestContext {
        VisualTestContext::from_window(self.window().into(), self.cx)
    }

    /// Whether the panel's terminal holds its window's keyboard.
    fn terminal_focused(&mut self) -> bool {
        let handle = self.window();
        self.cx
            .update(|cx| {
                handle.update(cx, |view, window, cx| {
                    let focus = gpui_kit::Focusable::focus_handle(view.terminal().read(cx), cx);
                    focus.is_focused(window)
                })
            })
            .unwrap()
    }

    fn quick_text(&self) -> Option<String> {
        let pane = self.panel.shared()?.pane()?;
        pane.read_text(ngterm_term::TextRegion::Viewport).ok()
    }

    /// Gives the panel's window the keyboard.
    fn activate(&mut self) {
        let mut visual = self.visual();
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        self.turn();
    }

    /// Lets the key monitor's delayed keyboard-left check run.
    fn pass_leave_check(&mut self) {
        self.cx.executor().advance_clock(Duration::from_millis(500));
        self.turn();
    }

    /// The focus reports `cat -v` echoed after the program's `ready`.
    fn reports(&self) -> String {
        let text = self.quick_text().unwrap_or_default();
        text.split_once("ready")
            .map(|(_, after)| after.trim().to_owned())
            .unwrap_or_default()
    }
}

#[gpui_kit::test]
fn the_panel_shows_its_terminal_and_keeps_the_shell_across_hide_and_show(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    h.turn();
    assert!(h.panel.window().is_none(), "nothing before the first show");
    assert!(!h.state.core.quick_has_surface());

    h.state.core.toggle_quick_terminal();
    h.turn();
    let first = h.window();
    assert!(
        h.state.core.quick_has_surface(),
        "the terminal is installed in the core"
    );
    assert_eq!(h.panel.frame().map(|frame| frame.host), Some(QuickHost::Window));
    h.until("the shell to start", |h| {
        h.panel.shared().is_some_and(|shared| shared.is_realized())
    });
    assert!(h.terminal_focused(), "the terminal has the panel's keyboard");
    let launched = h.launches.lock().unwrap().clone();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].working_directory, Some(PathBuf::from("/home/u")));
    let key = h.panel.shared().unwrap().key();

    // hidden: the window goes, the shell stays typable and readable
    h.state.core.toggle_quick_terminal();
    h.turn();
    assert!(h.panel.window().is_none());
    assert!(
        h.cx.update(|cx| first.update(cx, |_, _, _| ()).is_err()),
        "the window is removed"
    );
    let shared = Rc::clone(h.panel.shared().unwrap());
    assert!(!shared.is_torn_down());
    shared.pane().unwrap().write(b"typed while hidden\n").unwrap();
    h.until("the echo of the hidden shell", |h| {
        h.quick_text().is_some_and(|text| text.contains("typed while hidden"))
    });

    // shown again: a new window over the same terminal, focused
    h.state.core.toggle_quick_terminal();
    h.turn();
    assert_ne!(h.window(), first);
    assert_eq!(h.panel.shared().unwrap().key(), key, "the same shell");
    assert_eq!(h.launches.lock().unwrap().len(), 1, "never a second launch");
    h.until("the new view to focus", Harness::terminal_focused);
}

#[gpui_kit::test]
fn the_panel_goes_with_its_shell_and_the_next_show_starts_afresh(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    h.state.core.toggle_quick_terminal();
    h.turn();
    h.until("the shell to start", |h| {
        h.panel.shared().is_some_and(|shared| shared.is_realized())
    });
    let key = h.panel.shared().unwrap().key();
    // the shell ends: the panel goes with its terminal
    h.panel.shared().unwrap().pane().unwrap().write(&[4]).unwrap();
    h.until("the panel to go with its shell", |h| h.panel.window().is_none());
    assert!(h.panel.shared().is_none());
    assert!(!h.state.core.quick_visible() && !h.state.core.quick_has_surface());
    h.state.core.toggle_quick_terminal();
    h.turn();
    assert_eq!(h.launches.lock().unwrap().len(), 2);
    assert_ne!(h.panel.shared().unwrap().key(), key, "a fresh shell");
}

#[gpui_kit::test]
fn losing_the_keyboard_dismisses_a_summoned_panel_but_not_a_pinned_one(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    h.state.core.toggle_quick_terminal();
    h.turn();
    let mut visual = h.visual();
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    h.turn();
    assert!(h.cx.update(|cx| holds_keyboard(cx)), "the panel holds the keyboard");
    let mut visual = h.visual();
    visual.deactivate_window();
    h.turn();
    assert!(!h.state.core.quick_visible(), "a summoned panel is dismissed");
    assert!(h.panel.window().is_none());
    assert!(!h.cx.update(|cx| holds_keyboard(cx)));
    assert!(h.state.core.quick_has_surface(), "its shell kept");

    // a control show pins it
    let shown = h.state.handle_request(&ngterm_proto::Request {
        cmd: ngterm_proto::Command::Quick(ngterm_proto::command::QuickCmd::Visibility),
        target: None,
        args: Some(ngterm_proto::Args {
            mode: Some("show".to_owned()),
            ..ngterm_proto::Args::default()
        }),
    });
    drop(shown);
    h.state.settle();
    h.turn();
    let mut visual = h.visual();
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    visual.deactivate_window();
    h.turn();
    assert!(h.state.core.quick_visible(), "a pinned panel stays");
    assert!(h.panel.window().is_some());
}

#[gpui_kit::test]
fn the_window_managers_close_hides_the_panel_with_its_shell_kept(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    h.state.core.toggle_quick_terminal();
    h.turn();
    let mut visual = h.visual();
    assert!(!visual.simulate_close(), "the panel closes its window itself");
    h.turn();
    assert!(!h.state.core.quick_visible());
    assert!(h.panel.window().is_none());
    assert!(h.state.core.quick_has_surface());
}

#[gpui_kit::test]
fn a_hide_the_panel_starts_itself_forgets_the_keys_the_keymap_consumed(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    crate::keymap_dispatch::sync(&h.state.core, &h.env.poster);
    h.state.core.toggle_quick_terminal();
    h.turn();
    h.activate();
    assert!(h.cx.update(|cx| holds_keyboard(cx)));
    // the toggle's chord in the panel: the keymap consumes it and the panel hides itself
    let mut visual = h.visual();
    visual.simulate_keystrokes("ctrl-`");
    h.turn();
    assert!(!h.state.core.quick_visible(), "the chord hid the panel");
    assert!(h.panel.window().is_none());
    // the key-up goes to whatever takes the keyboard next, another application here: once no ngterm window holds
    // it the press is forgotten, so the key's next press is no autorepeat to swallow
    h.pass_leave_check();
    assert!(
        !crate::keymap_dispatch::release_simulated("ctrl-`"),
        "the consumed press was forgotten"
    );
}

#[gpui_kit::test]
fn a_hide_reports_the_focus_leaving_and_the_next_show_its_return(cx: &mut TestAppContext) {
    let script = "stty -echo -icanon; printf '\\033[?1004hready\\r\\n'; exec cat -v";
    let mut h = harness_running(cx, script);
    h.state.core.toggle_quick_terminal();
    h.turn();
    h.activate();
    h.until("the program to enable focus reports", |h| {
        h.quick_text().is_some_and(|text| text.contains("ready"))
    });
    h.until("the view to take the keyboard", Harness::terminal_focused);
    assert!(!h.reports().contains("^[[O"), "{}", h.reports());

    // hidden: the window is removed with no blur or deactivation, and the program is told the focus left
    h.state.core.toggle_quick_terminal();
    h.turn();
    assert!(h.panel.window().is_none());
    h.until("the focus-out report", |h| h.reports().ends_with("^[[O"));

    // shown again: the new window's view reports the focus coming back
    h.state.core.toggle_quick_terminal();
    h.turn();
    h.activate();
    h.until("the focus-in report", |h| h.reports().ends_with("^[[O^[[I"));
    assert_eq!(h.reports().matches("^[[O").count(), 1, "{}", h.reports());
}

#[gpui_kit::test]
fn a_window_the_panel_is_closing_loses_the_keyboard_without_a_dismissal(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    // summoned, so a focus loss would dismiss it
    h.state.core.toggle_quick_terminal();
    h.turn();
    h.activate();
    assert!(h.cx.update(|cx| holds_keyboard(cx)));
    // the keyboard leaving a window the panel closes for a reopen (the layer host's zoom or refit) is the reopen's,
    // not a focus loss: the panel stays
    h.panel.shown.as_ref().unwrap().links.closing.set(true);
    let mut visual = h.visual();
    visual.deactivate_window();
    h.turn();
    assert!(h.state.core.quick_visible(), "the reopen is no focus loss");
    assert!(h.panel.window().is_some());
}
