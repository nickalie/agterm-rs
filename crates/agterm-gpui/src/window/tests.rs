//! The window shell: its title (ported from the GTK build's `window.rs` tests), the state `window.list` reads, and the
//! shell over a deck in GPUI's test window.

use agterm_core::app_actions::FrontendAction;
use agterm_core::model::WindowLibrary;
use agterm_core::{CoreConfig, CoreState};
use agterm_proto::AppIdentity;
use gpui_kit::{TestAppContext, px, size};

use super::*;
use crate::deck::DeckEnv;
use crate::pane::PaneContext;
use crate::terminal::TerminalConfig;

#[test]
fn the_title_is_the_active_session_then_the_custom_window_name() {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    assert_eq!(window_title(&library, Some(window)), "u");
    library.rename_window(window, "work");
    assert_eq!(window_title(&library, Some(window)), "u — work");
    library.store_mut(window).unwrap().select_session(None);
    assert_eq!(window_title(&library, Some(window)), "Agterm — work");
    assert_eq!(window_title(&library, None), "Agterm");
}

#[test]
fn the_dashboard_title_names_a_custom_window() {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    assert_eq!(dashboard_title(&library, window), "Dashboard");
    library.rename_window(window, "work");
    assert_eq!(dashboard_title(&library, window), "Dashboard — work");
}

#[test]
fn maximized_is_zoomed_only_outside_full_screen_and_minimized_is_never_read() {
    let display = Some(WindowSize {
        width: 1280,
        height: 800,
    });
    let maximized = Painted {
        fullscreen: false,
        maximized: true,
    };
    assert_eq!(
        maximized.state(display),
        WindowState {
            fullscreen: false,
            zoomed: true,
            minimized: None,
            display,
        }
    );
    let both = Painted {
        fullscreen: true,
        maximized: true,
    };
    assert!(both.state(None).fullscreen && !both.state(None).zoomed);
    assert_eq!(Painted::default().state(None).display, None);
}

fn core(library: WindowLibrary) -> Core {
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            ..CoreState::default()
        },
    )
}

#[test]
fn the_terminal_area_starts_past_a_shown_sidebar() {
    let library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let mut core = core(library);
    let width = core.library().store(window).unwrap().sidebar().width;
    assert!((terminal_area_inset(&core, window) - (width + 1.0)).abs() < f64::EPSILON);
    core.library_mut().store_mut(window).unwrap().set_sidebar_visible(false);
    assert!(terminal_area_inset(&core, window).abs() < f64::EPSILON);
}

#[gpui_kit::test]
fn the_shell_titles_its_window_and_keeps_its_modal_layers_in_order(cx: &mut TestAppContext) {
    // the sidebar's footer draws gpui-kit buttons, which read its theme
    cx.update(gpui_kit::init);
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    library.rename_window(window, "work");
    let mut core = core(library);
    let state_dir = tempfile::tempdir().unwrap();
    let launcher: crate::pane::Launcher = Rc::new(|_: &agterm_term::Launch| -> crate::terminal::Spawn {
        Box::new(|_| Err(agterm_term::PaneError::Closed))
    });
    let env = Rc::new(DeckEnv::new(
        PaneContext {
            socket: "/tmp/agterm-test.sock".to_owned(),
            program_version: "1.2.3".to_owned(),
        },
        launcher,
        TerminalConfig::default(),
        crate::state::detached_poster(),
        crate::deck::Watermarks {
            state_dir: state_dir.path().to_path_buf(),
            config_with: Rc::new(|_: &str| TerminalConfig::default()),
        },
    ));
    let poster = env.poster.clone();
    let keys = poster.clone();
    let (view, cx) = cx.add_window_view(move |root, cx| {
        let deck = cx.new(|_| Deck::new(window, env));
        WindowView::new(window, deck, poster, 0, root, cx)
    });
    cx.simulate_resize(size(px(640.), px(480.)));
    let _ = view.update_in(cx, |view, window, cx| view.sync(&mut core, window, cx));
    cx.run_until_parked();
    // the title bar, its hairline along its bottom edge, spans the window over the sidebar's column too
    let bar = cx.debug_bounds("titlebar").expect("the title bar is laid out");
    let sidebar = cx.debug_bounds("sidebar").expect("the sidebar shows");
    assert_eq!((bar.left(), bar.size.width), (px(0.), px(640.)));
    assert!(sidebar.left() >= bar.left() && sidebar.right() <= bar.right());
    assert!(sidebar.top() >= bar.bottom(), "the sidebar starts under the hairline");
    view.read_with(cx, |view, _| {
        assert_eq!(view.title(), "u — work");
        assert!(!view.has_layer(Layer::Ask));
    });
    let session = view.read_with(cx, |view, cx| view.deck().read(cx).shown());
    assert!(session.is_some(), "the deck shows the selected session");
    let (ask, picker) = (cx.new(|_| Blank).into(), cx.new(|_| Blank).into());
    view.update(cx, |view, cx| {
        view.set_layer(Layer::Picker, Some(picker), cx);
        view.set_layer(Layer::Ask, Some(ask), cx);
    });
    view.read_with(cx, |view, _| {
        let order: Vec<Layer> = view.layers.iter().map(|(layer, _)| *layer).collect();
        assert_eq!(
            order,
            [Layer::Search, Layer::Ask, Layer::Picker, Layer::Switcher],
            "back to front, whatever the attach order (the search bar and the switcher for good)"
        );
    });
    view.update(cx, |view, cx| view.set_layer(Layer::Ask, None, cx));
    view.read_with(cx, |view, _| {
        assert!(!view.has_layer(Layer::Ask) && view.has_layer(Layer::Picker));
    });

    // the key monitor watches the window while it is open
    let handle = cx.update(|root, _| root.window_handle());
    let origin = crate::keymap_dispatch::KeyOrigin::Window(window);
    assert_eq!(crate::keymap_dispatch::window_of(origin), Some(handle));
    assert!(crate::keymap_dispatch::holds_keyboard(Some(handle)));
    assert!(!crate::keymap_dispatch::holds_keyboard(None));

    let workspace = core.library().store(window).unwrap().workspaces()[0].id();
    keymap_renames_reach_the_sidebar(cx, &view, &keys, (window, session.unwrap(), workspace));
    view.update_in(cx, |view, root, cx| view.close(root, cx));
    assert_eq!(crate::keymap_dispatch::window_of(origin), None);
    assert!(!crate::keymap_dispatch::holds_keyboard(Some(handle)));
}

/// The keymap's Rename Session and Rename Workspace reach the window's sidebar, and its field keeps the keys the key
/// monitor sees: editing keys, Enter and Escape. `(window, session, workspace)`: the model window and its rows.
fn keymap_renames_reach_the_sidebar(
    cx: &mut gpui_kit::VisualTestContext,
    view: &Entity<WindowView>,
    keys: &Poster,
    (window, session, workspace): (Uuid, Uuid, Uuid),
) {
    cx.update(|_, cx| crate::keymap_dispatch::init(cx));
    let sidebar = view.read_with(cx, |view, _| view.sidebar().clone());
    let editing = |cx: &mut gpui_kit::VisualTestContext| sidebar.read_with(cx, |sidebar, _| sidebar.editing());
    // from the app, as the landing runs it: the window is not being updated
    let rename = FrontendAction::RenameSession { window, session };
    cx.cx
        .update(|cx| crate::keymap_actions::perform(keys, rename, false, cx));
    cx.run_until_parked();
    assert_eq!(editing(cx), Some(session));
    cx.simulate_keystrokes("o p s backspace");
    let text = sidebar.read_with(cx, Sidebar::edited_text);
    assert_eq!(text.as_deref(), Some("op"));
    cx.simulate_keystrokes("enter");
    assert_eq!(editing(cx), None, "Enter committed");
    let rename = FrontendAction::RenameWorkspace { window, workspace };
    cx.cx
        .update(|cx| crate::keymap_actions::perform(keys, rename, false, cx));
    cx.run_until_parked();
    assert_eq!(editing(cx), Some(workspace));
    cx.simulate_keystrokes("escape");
    assert_eq!(editing(cx), None, "Escape cancelled");
}

/// A layer view that draws nothing.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

// MARK: - dialogs over the window (Task 21)

/// A loop that keeps what is posted for the test to run.
#[derive(Default)]
struct Posted(std::sync::Mutex<Vec<agterm_host::MainTask<AppState>>>);

impl agterm_host::MainLoop<AppState> for Posted {
    fn post(&self, task: agterm_host::MainTask<AppState>) -> Result<(), agterm_host::MainLoopClosed> {
        self.0.lock().unwrap().push(task);
        Ok(())
    }

    fn post_after(&self, _: Duration, _: agterm_host::MainTask<AppState>) -> Result<(), agterm_host::MainLoopClosed> {
        Ok(())
    }
}

/// A window over a real core whose posts the test runs, as the app's loop and the windows' sync do.
struct Shell<'a> {
    state: AppState,
    posted: std::sync::Arc<Posted>,
    view: Entity<WindowView>,
    window: Uuid,
    cx: &'a mut gpui_kit::VisualTestContext,
    _state_dir: tempfile::TempDir,
}

/// Workspace "workspace 1" with sessions "u" and "var" (selected); closing a session asks first.
fn shell(cx: &mut TestAppContext) -> Shell<'_> {
    cx.update(gpui_kit::init);
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let var = store
        .add_session(workspace, agterm_core::model::NewSession::at_cwd("/var"))
        .unwrap();
    store.select_session(Some(var));
    let core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings: agterm_core::persist::AppSettings {
                confirm_close_session: Some(true),
                close_grace_undo_enabled: Some(false),
                ..agterm_core::persist::AppSettings::default()
            },
            ..CoreState::default()
        },
    );
    let posted = std::sync::Arc::new(Posted::default());
    let weak: Poster = std::sync::Arc::downgrade(&posted) as Poster;
    let mut state = AppState::new(core);
    state.attach(weak.clone());
    let state_dir = tempfile::tempdir().unwrap();
    let launcher: crate::pane::Launcher = Rc::new(|_: &agterm_term::Launch| -> crate::terminal::Spawn {
        Box::new(|_| Err(agterm_term::PaneError::Closed))
    });
    let env = Rc::new(DeckEnv::new(
        PaneContext {
            socket: "/tmp/agterm-test.sock".to_owned(),
            program_version: "1.2.3".to_owned(),
        },
        launcher,
        TerminalConfig::default(),
        weak.clone(),
        crate::deck::Watermarks {
            state_dir: state_dir.path().to_path_buf(),
            config_with: Rc::new(|_: &str| TerminalConfig::default()),
        },
    ));
    let (view, cx) = cx.add_window_view(move |root, cx| {
        let deck = cx.new(|_| Deck::new(window, env));
        WindowView::new(window, deck, weak, 0, root, cx)
    });
    cx.simulate_resize(size(px(800.), px(500.)));
    cx.update(|root, _| root.activate_window());
    let mut shell = Shell {
        state,
        posted,
        view,
        window,
        cx,
        _state_dir: state_dir,
    };
    shell.settle();
    shell
}

impl Shell<'_> {
    /// Runs what was posted, then syncs the window, until nothing more is posted.
    fn settle(&mut self) {
        for _ in 0..12 {
            self.cx.run_until_parked();
            let tasks = std::mem::take(&mut *self.posted.0.lock().unwrap());
            for task in tasks {
                task(&mut self.state);
            }
            let (view, core) = (self.view.clone(), &mut self.state.core);
            let focused = view.update_in(self.cx, |view, window, cx| view.sync(core, window, cx));
            if focused {
                self.state.settle();
            }
            self.cx.run_until_parked();
            if self.posted.0.lock().unwrap().is_empty() {
                return;
            }
        }
    }

    /// Whether the shown terminal holds the window's keyboard.
    fn terminal_focused(&mut self) -> bool {
        let view = self.view.clone();
        self.cx.update(|window, cx| {
            let deck = view.read(cx).deck().read(cx);
            deck.shown_terminal()
                .is_some_and(|pane| pane.focus_handle(cx).is_focused(window))
        })
    }

    fn alert_open(&mut self) -> bool {
        self.cx
            .update(|window, cx| crate::modal::alert_open(window.window_handle(), cx))
    }

    fn session(&self, name: &str) -> Option<Uuid> {
        let store = self.state.core.library().store(self.window)?;
        store
            .sessions()
            .find(|session| session.display_name() == name)
            .map(Session::id)
    }

    /// Opens `var`'s row menu with a right-click, its Close Session's confirmation, and the menu's dismissal, as a
    /// click on the item runs them.
    fn close_var_from_its_menu(&mut self) {
        let row = self.cx.debug_bounds("row:var").expect("the row is laid out").center();
        self.cx
            .simulate_mouse_down(row, gpui_kit::MouseButton::Right, gpui_kit::Modifiers::default());
        self.cx
            .simulate_mouse_up(row, gpui_kit::MouseButton::Right, gpui_kit::Modifiers::default());
        self.cx.run_until_parked();
        let sidebar = self.view.read_with(self.cx, |view, _| view.sidebar().clone());
        assert!(
            sidebar.read_with(self.cx, |sidebar, _| sidebar.menu_open()),
            "the menu holds the keyboard"
        );
        let var = self.session("var").unwrap();
        sidebar.update_in(self.cx, |sidebar, window, cx| {
            sidebar.close_sessions(vec![var], window, cx);
            sidebar.dismiss_menu(cx);
        });
        self.settle();
    }
}

#[gpui_kit::test]
fn a_cancelled_confirmation_from_a_row_menu_gives_the_terminal_the_keyboard_back(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    assert!(s.terminal_focused(), "the shown terminal has the keyboard");
    s.close_var_from_its_menu();
    assert!(s.alert_open(), "Close Session asks first");
    assert!(!s.terminal_focused(), "the alert holds the keyboard");
    // Return answers with the default, Cancel
    s.cx.simulate_keystrokes("enter");
    s.settle();
    assert!(!s.alert_open());
    assert!(s.session("var").is_some(), "cancelled");
    assert!(s.terminal_focused(), "the terminal has the keyboard back");

    // Escape cancels too; then the confirmed close moves the keyboard to the next shown terminal
    s.close_var_from_its_menu();
    s.cx.simulate_keystrokes("escape");
    s.settle();
    assert!(s.session("var").is_some() && s.terminal_focused());
    s.close_var_from_its_menu();
    s.cx.simulate_keystrokes("tab enter");
    s.settle();
    assert!(s.session("var").is_none(), "confirmed");
    assert!(s.terminal_focused(), "the session shown now has the keyboard");
}

/// Opens a GUI ask "Ship it?" with buttons Ship and Hold; its dialog.
fn open_gui_ask(s: &mut Shell<'_>) -> Entity<AskDialog> {
    let response = s.state.core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Ask(agterm_proto::command::AskCmd::Open),
        target: None,
        args: Some(agterm_proto::Args {
            title: Some("Ship it?".to_owned()),
            style: Some("gui".to_owned()),
            buttons: Some(vec![
                agterm_proto::AskButton {
                    id: "ship".to_owned(),
                    label: "Ship".to_owned(),
                    hotkey: None,
                },
                agterm_proto::AskButton {
                    id: "hold".to_owned(),
                    label: "Hold".to_owned(),
                    hotkey: None,
                },
            ]),
            ..agterm_proto::Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    s.state.settle();
    s.settle();
    s.view
        .read_with(s.cx, |view, _| view.ask_dialog().cloned())
        .expect("the GUI ask is up")
}

/// Whether `dialog` holds the window's keyboard.
fn ask_held(s: &mut Shell<'_>, dialog: &Entity<AskDialog>) -> bool {
    s.cx.update(|window, cx| dialog.read(cx).focus_handle().is_focused(window))
}

/// Opens a clipboard prompt over the window, as a terminal's OSC 52 read does; the buttons it is answered with land
/// in `answers`.
fn open_prompt(s: &mut Shell<'_>, answers: &Rc<std::cell::RefCell<Vec<usize>>>) {
    let answers = Rc::clone(answers);
    let spec = crate::terminal::prompt_alert(agterm_core::clipboard_policy::ClipboardAccess::Read);
    s.cx.update(|window, cx| {
        crate::modal::open_alert(
            window,
            spec,
            move |answer, _, _| answers.borrow_mut().push(answer.button),
            cx,
        );
    });
    s.settle();
}

#[gpui_kit::test]
fn a_gui_ask_holds_the_keyboard_until_answered(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let dialog = open_gui_ask(&mut s);
    assert!(s.view.read_with(s.cx, |view, _| view.has_layer(Layer::Ask)));
    assert!(
        ask_held(&mut s, &dialog) && !s.terminal_focused(),
        "the dialog holds the keyboard"
    );
    let id = dialog.read_with(s.cx, |dialog, _| dialog.id().to_owned());
    s.cx.simulate_keystrokes("right enter");
    s.settle();
    assert!(!s.state.core.modal_pending(s.window), "the answer freed the slot");
    assert!(s.view.read_with(s.cx, |view, _| view.ask_dialog().is_none()
        && !view.has_layer(Layer::Ask)));
    let result = s.state.core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Ask(agterm_proto::command::AskCmd::Result),
        target: Some(id),
        args: None,
    });
    assert!(format!("{:?}", result.result).contains("\"hold\""), "{result:?}");
    assert!(s.terminal_focused(), "the terminal has the keyboard back");
}

#[gpui_kit::test]
fn a_gui_ask_takes_the_keyboard_back_from_an_alert_over_it(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let dialog = open_gui_ask(&mut s);
    let answers = Rc::default();
    open_prompt(&mut s, &answers);
    assert!(
        s.alert_open() && !ask_held(&mut s, &dialog),
        "the prompt holds the keyboard"
    );
    s.cx.simulate_keystrokes("escape");
    s.settle();
    assert_eq!(answers.borrow().as_slice(), [0], "denied");
    assert!(!s.alert_open());
    assert!(ask_held(&mut s, &dialog), "the GUI ask has the keyboard back");
    // so its keys reach it again
    s.cx.simulate_keystrokes("enter");
    s.settle();
    assert!(!s.state.core.modal_pending(s.window), "the ask is answered");
}

#[gpui_kit::test]
fn a_closing_window_cancels_its_alerts(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let answers = Rc::default();
    open_prompt(&mut s, &answers);
    open_prompt(&mut s, &answers);
    let handle = s.cx.update(|window, _| window.window_handle());
    let view = s.view.clone();
    view.update_in(s.cx, |view, window, cx| view.close(window, cx));
    assert_eq!(
        answers.borrow().as_slice(),
        [0, 0],
        "both prompts, the shown one and the queued one, are denied"
    );
    assert!(
        !s.cx.cx.update(|cx| crate::modal::alert_open(handle, cx)),
        "none is left"
    );
}

#[gpui_kit::test]
fn a_popover_and_rename_window_give_the_keyboard_back(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let popovers = s.view.read_with(s.cx, |view, _| view.popovers().clone());
    s.cx.update(|window, cx| {
        popovers.update(cx, |popovers, cx| {
            popovers.toggle(
                crate::popovers::PopoverKind::Recent,
                gpui_kit::point(px(600.), px(48.)),
                window,
                cx,
            );
        });
    });
    s.settle();
    assert!(s.view.read_with(s.cx, |view, _| view.has_layer(Layer::Popover)));
    assert!(!s.terminal_focused(), "the popover holds the keyboard");
    s.cx.simulate_keystrokes("escape");
    s.settle();
    assert!(!s.view.read_with(s.cx, |view, _| view.has_layer(Layer::Popover)));
    assert!(s.terminal_focused(), "closed, it hands the keyboard back");

    let window = s.window;
    let rename = FrontendAction::RenameWindow {
        window,
        name: "work".to_owned(),
    };
    let poster = s.state.poster().clone();
    s.cx.cx
        .update(|cx| crate::keymap_actions::perform(&poster, rename, false, cx));
    s.settle();
    assert!(s.alert_open() && !s.terminal_focused());
    s.cx.simulate_keystrokes("p l a y");
    s.cx.simulate_keystrokes("enter");
    s.settle();
    let name = s
        .state
        .core
        .library()
        .custom_window_name(window)
        .map(ToString::to_string);
    assert_eq!(name.as_deref(), Some("play"));
    assert!(s.terminal_focused(), "renamed, the terminal has the keyboard back");
}

// MARK: - picker, palette, switcher and search bar (Task 22)

impl Shell<'_> {
    /// Puts the key monitor on the app and loads the core's keymap, as the app's launch does.
    fn with_keys(&mut self) {
        let poster = self.state.poster().clone();
        self.cx.update(|_, cx| crate::keymap_dispatch::init(cx));
        crate::keymap_dispatch::sync(&self.state.core, &poster);
    }

    /// Whether `handle` of the window's views holds the keyboard.
    fn holds(&mut self, handle: impl FnOnce(&WindowView, &App) -> Option<gpui_kit::FocusHandle>) -> bool {
        let view = self.view.clone();
        self.cx
            .update(|window, cx| handle(view.read(cx), cx).is_some_and(|handle| handle.is_focused(window)))
    }

    fn active(&self) -> Option<Uuid> {
        self.state
            .core
            .library()
            .store(self.window)
            .and_then(AppStore::active_session)
            .map(Session::id)
    }

    /// Runs `apply` on the active session's search, then lands it.
    fn search(&mut self, apply: impl FnOnce(&mut agterm_core::model::SearchState)) {
        let (window, session) = (self.window, self.active().unwrap());
        let session = self
            .state
            .core
            .library_mut()
            .store_mut(window)
            .and_then(|store| store.session_mut(session))
            .unwrap();
        apply(&mut session.search);
        self.state.settle();
        self.settle();
    }
}

/// Opens a picker of Alpha and Beta; its id.
fn open_pick(s: &mut Shell<'_>) -> String {
    let item = |id: &str, label: &str| agterm_proto::PickItem {
        id: id.to_owned(),
        label: label.to_owned(),
        subtitle: None,
    };
    let response = s.state.core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Pick(agterm_proto::command::PickCmd::Open),
        target: None,
        args: Some(agterm_proto::Args {
            items: Some(vec![item("alpha", "Alpha"), item("beta", "Beta")]),
            ..agterm_proto::Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    s.state.settle();
    s.settle();
    let picker = s.view.read_with(s.cx, |view, _| view.picker().cloned());
    picker
        .expect("the picker is up")
        .read_with(s.cx, |picker, _| picker.id().to_owned())
}

fn pick_result(s: &mut Shell<'_>, id: &str) -> String {
    let result = s.state.core.dispatch(&agterm_proto::Request {
        cmd: agterm_proto::Command::Pick(agterm_proto::command::PickCmd::Result),
        target: Some(id.to_owned()),
        args: None,
    });
    format!("{:?}", result.result)
}

#[gpui_kit::test]
fn a_picker_takes_the_keyboard_answers_by_its_keys_and_gives_it_back(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    s.with_keys();
    let id = open_pick(&mut s);
    assert!(s.view.read_with(s.cx, |view, _| view.has_layer(Layer::Picker)));
    assert!(
        s.holds(|view, cx| view.picker().map(|picker| picker.read(cx).focus_handle(cx))),
        "its field holds the keyboard"
    );
    assert!(!s.terminal_focused());
    s.cx.simulate_keystrokes("down enter");
    s.settle();
    assert!(!s.state.core.modal_pending(s.window), "answered");
    assert!(pick_result(&mut s, &id).contains("beta"));
    assert!(s.view.read_with(s.cx, |view, _| view.picker().is_none()
        && !view.has_layer(Layer::Picker)));
    assert!(s.terminal_focused(), "the terminal has the keyboard back");

    // Ctrl+J moves too, typing filters in the field, Escape cancels
    let id = open_pick(&mut s);
    s.cx.simulate_keystrokes("ctrl-j ctrl-k b");
    let rows = s
        .view
        .read_with(s.cx, |view, cx| view.picker().unwrap().read(cx).rows().to_vec());
    assert_eq!(rows, [crate::picker::PickRow::Item(1)], "typed into the field");
    s.cx.simulate_keystrokes("escape");
    s.settle();
    let result = pick_result(&mut s, &id);
    assert!(result.to_lowercase().contains("cancelled"), "{result}");
    assert!(s.terminal_focused());
}

/// Opens the palette of `mode` in the shell's window.
fn open_palette(s: &mut Shell<'_>, mode: agterm_core::palette::PaletteMode) {
    s.state.core.open_palette(s.window, mode);
    s.state.settle();
    s.settle();
}

#[gpui_kit::test]
fn a_palette_takes_the_keyboard_runs_a_row_and_its_launcher_closes_it(cx: &mut TestAppContext) {
    use agterm_core::palette::PaletteMode;

    let mut s = shell(cx);
    s.with_keys();
    open_palette(&mut s, PaletteMode::Sessions);
    assert!(s.view.read_with(s.cx, |view, _| view.has_layer(Layer::Palette)));
    assert!(s.holds(|view, cx| view.palette().map(|palette| palette.read(cx).focus_handle(cx))));
    s.cx.simulate_keystrokes("u");
    let titles = s
        .view
        .read_with(s.cx, |view, cx| view.palette().unwrap().read(cx).titles());
    assert_eq!(titles, ["u"]);
    s.cx.simulate_keystrokes("enter");
    s.settle();
    assert_eq!(s.active(), s.session("u"), "the row ran");
    assert!(s.view.read_with(s.cx, |view, _| view.palette().is_none()));
    assert!(s.terminal_focused(), "the terminal has the keyboard back");

    // the launcher's chord, typed in the field, closes it as the menu item toggles it
    let chord = s
        .state
        .core
        .keymap()
        .equivalent(agterm_core::keymap::BuiltinAction::CommandPalette)
        .cloned()
        .expect("Command Palette has a chord");
    open_palette(&mut s, PaletteMode::Actions);
    assert!(s.view.read_with(s.cx, |view, _| view.palette().is_some()));
    s.cx.simulate_keystrokes(&keystroke(&chord));
    s.settle();
    assert!(s.state.core.palette_mode().is_none(), "closed by its launcher");
    assert!(s.terminal_focused());

    // Escape closes it too
    open_palette(&mut s, PaletteMode::Actions);
    s.cx.simulate_keystrokes("escape");
    s.settle();
    assert!(s.state.core.palette_mode().is_none());
    assert!(s.terminal_focused());
}

/// `chord` in GPUI's keystroke syntax.
fn keystroke(chord: &agterm_core::keymap::Chord) -> String {
    use agterm_core::keymap::Modifiers;
    let mut text = String::new();
    for (bit, word) in [
        (Modifiers::CONTROL, "ctrl-"),
        (Modifiers::OPTION, "alt-"),
        (Modifiers::SHIFT, "shift-"),
        (Modifiers::COMMAND, "super-"),
    ] {
        if chord.mods.contains(bit) {
            text.push_str(word);
        }
    }
    text.push_str(&chord.key);
    text
}

#[gpui_kit::test]
fn ctrl_tab_walks_the_recent_sessions_and_releasing_control_selects(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    s.with_keys();
    let switcher = s.view.read_with(s.cx, |view, _| view.switcher().clone());
    assert_eq!(s.active(), s.session("var"));
    s.cx.simulate_keystrokes("ctrl-tab");
    switcher.read_with(s.cx, |switcher, _| {
        assert!(switcher.is_active());
        assert_eq!(switcher.titles(), ["var", "u"], "most recent first");
        assert_eq!(switcher.index(), 1, "on the previous session");
    });
    s.cx.run_until_parked();
    assert!(s.cx.debug_bounds("session-switcher").is_some());
    assert!(s.terminal_focused(), "the terminal keeps the keyboard");
    assert!(
        crate::keymap_dispatch::release_simulated("ctrl-tab"),
        "the walk's Tab release stays off the terminal"
    );
    s.cx.simulate_keystrokes("escape");
    assert!(
        !switcher.read_with(s.cx, |switcher, _| switcher.is_active()),
        "Escape gives up"
    );
    assert!(
        crate::keymap_dispatch::release_simulated("escape"),
        "the cancelling Escape's release stays off the terminal"
    );
    s.settle();
    assert_eq!(s.active(), s.session("var"));

    s.cx.simulate_keystrokes("ctrl-tab ctrl-tab ctrl-shift-tab");
    assert_eq!(
        switcher.read_with(s.cx, |switcher, _| switcher.index()),
        1,
        "wrapped and back"
    );
    s.cx.simulate_modifiers_change(gpui_kit::Modifiers::default());
    s.settle();
    assert!(!switcher.read_with(s.cx, |switcher, _| switcher.is_active()));
    assert_eq!(s.active(), s.session("u"), "Control's release selected it");
    assert!(s.terminal_focused(), "the session shown now has the keyboard");
}

#[gpui_kit::test]
fn the_search_bar_takes_the_keyboard_keeps_what_is_typed_and_gives_it_back(cx: &mut TestAppContext) {
    use agterm_core::model::{SearchReport, StatusPane};

    let mut s = shell(cx);
    s.with_keys();
    let session = s.active().unwrap();
    s.search(|search| {
        search.apply(StatusPane::Left, SearchReport::Started(Some("err".to_owned())));
    });
    let bar = s.view.read_with(s.cx, |view, _| view.search_bar().clone());
    assert!(bar.read_with(s.cx, |bar, _| bar.is_shown()));
    assert!(s.cx.debug_bounds("search-field").is_some() && s.cx.debug_bounds("search-counter").is_some());
    assert!(
        s.holds(|view, cx| Some(view.search_bar().read(cx).focus_handle(cx))),
        "the bar takes the keyboard"
    );
    let text = |s: &mut Shell<'_>| bar.read_with(s.cx, |bar, cx| bar.field().read(cx).value().to_string());
    assert_eq!(text(&mut s), "err", "the seed shows");
    s.cx.simulate_keystrokes("x");
    s.settle();
    let mut needle = String::new();
    s.search(|search| needle.clone_from(&search.needle));
    assert_eq!(needle, "errx", "typing sends the needle");
    assert_eq!(text(&mut s), "errx", "a settle does not rewind the field");
    let mut shown = String::new();
    s.search(|search| {
        search.apply(StatusPane::Left, SearchReport::Total(Some(7)));
        search.apply(StatusPane::Left, SearchReport::Selected(Some(2)));
        shown = search.display_text();
    });
    let counter = bar.read_with(s.cx, |bar, _| bar.counter().to_owned());
    assert!(!counter.is_empty());
    assert_eq!(counter, shown);

    // Escape asks the pane to end its search, whose end report gives the terminal the keyboard back
    s.cx.simulate_keystrokes("escape");
    s.settle();
    let token = s.view.read_with(s.cx, |view, cx| {
        view.deck().read(cx).shown_terminal().unwrap().token().to_owned()
    });
    s.state.core.report_search(session, &token, SearchReport::Ended);
    s.state.settle();
    s.settle();
    assert!(!bar.read_with(s.cx, |bar, _| bar.is_shown()));
    assert!(s.terminal_focused(), "the terminal has the keyboard back");
}

// MARK: - the app menu (Task 26)

impl Shell<'_> {
    fn click(&mut self, selector: &'static str) {
        let at = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.cx.simulate_click(at, gpui_kit::Modifiers::default());
        self.cx.run_until_parked();
    }

    fn menu_open(&mut self) -> bool {
        let bar = self.view.read_with(self.cx, |view, _| view.titlebar().clone());
        bar.read_with(self.cx, |bar, _| bar.menu_open())
    }
}

#[gpui_kit::test]
fn a_menu_item_runs_in_its_window_and_the_terminal_gets_the_keyboard_back(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let before = s.state.core.library().store(s.window).unwrap().sessions().count();
    s.click("app-menu-button");
    s.settle();
    assert!(s.menu_open(), "the menu holds the keyboard");
    assert!(!s.terminal_focused());
    // File is the first submenu: Down lands on it, Right opens it
    s.cx.simulate_keystrokes("down right");
    s.cx.run_until_parked();
    assert!(
        s.cx.debug_bounds("app-menu-item-editKeymap").is_some(),
        "File lists Edit Keymap…"
    );
    assert!(
        s.cx.debug_bounds("app-menu-item-editHooks").is_some(),
        "and Edit Hooks…"
    );
    s.click("app-menu-item-newSession");
    s.settle();
    assert!(!s.menu_open(), "an item closes the menu");
    let after = s.state.core.library().store(s.window).unwrap().sessions().count();
    assert_eq!(after, before + 1, "New Session ran in the menu's window");
    assert!(s.terminal_focused(), "the new session's terminal has the keyboard");
}

#[gpui_kit::test]
fn the_menus_settings_and_quit_reach_the_frontend(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    s.click("app-menu-button");
    s.click("app-menu-item-settings");
    s.settle();
    assert_eq!(
        s.state.take_frontend_work().settings,
        Some(crate::settings::SettingsHost::Window(s.window)),
        "Settings… opens the Settings popup over the menu's window"
    );
    s.click("app-menu-button");
    s.click("app-menu-item-quit");
    s.settle();
    let work = s.state.take_frontend_work();
    if agterm_core::is_ui_test_launch(&agterm_core::paths::ProcessEnv) {
        assert!(work.quit);
    } else {
        assert!(!work.quit, "Quit asks first");
        assert_eq!(
            work.quit_prompt.as_deref(),
            Some("This closes 1 window and 2 sessions, ending all running shells.")
        );
    }
}

// MARK: - the library's widgets in the chrome's colors

/// A fill as painted: its bounds in logical pixels and its color.
type Fill = (gpui_kit::Bounds<gpui_kit::Pixels>, gpui_kit::Hsla);

/// A terminal theme of `background` under `foreground` text.
fn themed(background: (u8, u8, u8), foreground: (u8, u8, u8)) -> TerminalConfig {
    let mut config = TerminalConfig::default();
    config.theme.background = agterm_term::Rgb::new(background.0, background.1, background.2);
    config.theme.foreground = agterm_term::Rgb::new(foreground.0, foreground.1, foreground.2);
    config
}

impl Shell<'_> {
    /// Paints the chrome, and the library's theme with it, in `config`'s colors, as the windows' sync does after a
    /// reload or an appearance flip; its colors.
    fn paint_chrome(&mut self, config: &TerminalConfig) -> agterm_core::chrome::ChromeColors {
        let core = &self.state.core;
        let colors = self.cx.cx.update(|cx| {
            crate::chrome::sync(core, config, cx);
            crate::chrome::current(cx)
        });
        self.settle();
        colors
    }

    /// Every fill the last frame painted, back to front.
    fn fills(&mut self) -> Vec<Fill> {
        self.cx.update(|window, _| {
            let scale = window.scale_factor();
            let logical = |value: gpui_kit::ScaledPixels| px(value.0 / scale);
            window
                .painted_quads()
                .into_iter()
                .filter_map(|quad| {
                    let fill = quad.background.as_solid().filter(|fill| fill.a > 0.0)?;
                    let bounds = gpui_kit::Bounds {
                        origin: gpui_kit::point(logical(quad.bounds.origin.x), logical(quad.bounds.origin.y)),
                        size: size(logical(quad.bounds.size.width), logical(quad.bounds.size.height)),
                    };
                    Some((bounds, fill))
                })
                .collect()
        })
    }

    /// The fills `act` brings up: those painted after it that were not before.
    fn new_fills(&mut self, act: impl FnOnce(&mut Self)) -> Vec<Fill> {
        let before = self.fills();
        act(self);
        self.cx.run_until_parked();
        self.fills().into_iter().filter(|fill| !before.contains(fill)).collect()
    }

    /// The color element `selector` shows under the pointer, over the chrome color `under`: the front fill hovering
    /// brings up over its center.
    fn hovered(&mut self, selector: &'static str, under: agterm_core::chrome::Rgb) -> gpui_kit::Hsla {
        let at = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        let fills = self.new_fills(|s| s.cx.simulate_mouse_move(at, None, gpui_kit::Modifiers::default()));
        let (_, fill) = fills
            .into_iter()
            .rfind(|(bounds, _)| bounds.contains(&at))
            .unwrap_or_else(|| panic!("hovering {selector} paints a fill"));
        // the pointer leaves for the terminal area, so the next hover starts from nothing
        self.cx.simulate_mouse_move(
            gpui_kit::point(px(700.), px(400.)),
            None,
            gpui_kit::Modifiers::default(),
        );
        self.cx.run_until_parked();
        crate::chrome::color(under, 1.0).blend(fill)
    }

    /// The panel a menu `open` brings up: the largest fill it paints.
    fn menu_panel(&mut self, open: impl FnOnce(&mut Self)) -> gpui_kit::Hsla {
        let area = |(bounds, _): &Fill| f32::from(bounds.size.width) * f32::from(bounds.size.height);
        let fills = self.new_fills(open);
        fills
            .into_iter()
            .max_by(|a, b| area(a).total_cmp(&area(b)))
            .expect("the menu paints its panel")
            .1
    }
}

/// The library's widgets (the ghost buttons' hover, the dropdown and context menus) paint in the chrome's colors on
/// its dark and light sides and after a flip either way, never the library's own light palette over a dark chrome.
#[gpui_kit::test]
fn the_librarys_hovers_and_menus_paint_in_the_chromes_dark_and_light(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let dark = themed((0x10, 0x20, 0x30), (0xe0, 0xe0, 0xe0));
    let light = themed((0xfa, 0xfa, 0xf5), (0x20, 0x20, 0x28));
    for (config, dark, when) in [
        (&dark, true, "dark"),
        (&light, false, "to light"),
        (&dark, true, "to dark"),
    ] {
        let colors = s.paint_chrome(config);
        let background = crate::chrome::color(colors.background, 1.0);
        for (selector, under) in [("add-session", colors.sidebar), ("app-menu-button", colors.background)] {
            let shown = s.hovered(selector, under);
            assert_eq!(shown.l < 0.5, dark, "{when}: {selector} hovered shows {shown:?}");
            assert_ne!(
                shown,
                crate::chrome::color(under, 1.0),
                "{when}: {selector}'s hover shows"
            );
        }
        // the app menu's dropdown, and a row under the pointer in it
        let panel = s.menu_panel(|s| s.click("app-menu-button"));
        assert_eq!(panel, background, "{when}: the app menu's panel");
        let shown = s.hovered("app-menu-item-settings", colors.background);
        assert_eq!(shown.l < 0.5, dark, "{when}: a menu row hovered shows {shown:?}");
        s.cx.simulate_keystrokes("escape");
        s.settle();
        assert!(!s.menu_open());
        // a sidebar row's context menu
        let row = s.cx.debug_bounds("row:var").expect("the row is laid out").center();
        let panel = s.menu_panel(|s| {
            s.cx.simulate_mouse_down(row, gpui_kit::MouseButton::Right, gpui_kit::Modifiers::default());
            s.cx.simulate_mouse_up(row, gpui_kit::MouseButton::Right, gpui_kit::Modifiers::default());
        });
        assert_eq!(panel, background, "{when}: the row's context menu");
        s.cx.simulate_keystrokes("escape");
        s.settle();
    }
}

// MARK: - the Settings popup

impl Shell<'_> {
    /// Opens the Settings popup over the window, as the landing of Settings… does.
    fn open_settings(&mut self) {
        let opening = crate::settings::Opening::of(&self.state);
        self.view
            .update_in(self.cx, |view, window, cx| view.open_settings(opening, window, cx));
        self.settle();
    }

    fn settings(&mut self) -> Option<Entity<SettingsView>> {
        self.view.read_with(self.cx, |view, _| view.settings().cloned())
    }

    /// Whether the Settings popup is up and holds the keyboard.
    fn settings_held(&mut self) -> bool {
        let Some(view) = self.settings() else {
            return false;
        };
        self.cx.update(|window, cx| view.read(cx).holds_keyboard(window, cx))
    }

    fn settings_closed(&mut self) -> bool {
        let layer = self.view.read_with(self.cx, |view, _| view.has_layer(Layer::Settings));
        self.settings().is_none() && !layer && self.cx.debug_bounds("settings").is_none()
    }
}

#[gpui_kit::test]
fn the_settings_popup_opens_inside_its_window_holds_the_keyboard_and_escape_closes_it(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    s.with_keys();
    assert!(s.terminal_focused());
    s.click("app-menu-button");
    s.click("app-menu-item-settings");
    s.settle();
    assert!(s.state.take_frontend_work().settings.is_some());
    s.open_settings();
    assert!(s.view.read_with(s.cx, |view, _| view.has_layer(Layer::Settings)));
    // centered in the 800x500 window, upstream's 540 wide and shrunk to the window's height less the margins
    let panel = s.cx.debug_bounds("settings").expect("the popup is drawn in the window");
    assert_eq!((panel.size.width, panel.size.height), (px(540.), px(468.)));
    assert_eq!(panel.center(), gpui_kit::point(px(400.), px(250.)));
    assert!(s.cx.debug_bounds("settings-tab-general").is_some(), "its tabs");
    assert!(
        s.settings_held() && !s.terminal_focused(),
        "the popup holds the keyboard"
    );
    // its keys are its own: no Ctrl-Tab walk, no keymap chord, nothing typed into the terminal
    let switcher = s.view.read_with(s.cx, |view, _| view.switcher().clone());
    s.cx.simulate_keystrokes("ctrl-tab");
    assert!(!switcher.read_with(s.cx, |switcher, _| switcher.is_active()));
    let chord = s
        .state
        .core
        .keymap()
        .equivalent(agterm_core::keymap::BuiltinAction::CommandPalette)
        .cloned()
        .expect("Command Palette has a chord");
    let _ = crate::keymap_dispatch::take_fired();
    s.cx.simulate_keystrokes(&keystroke(&chord));
    s.settle();
    assert!(
        crate::keymap_dispatch::take_fired().is_empty(),
        "the keymap is not reached"
    );
    assert!(s.state.core.palette_mode().is_none());
    assert!(s.settings_held());
    // the window's sync keeps the keyboard on it
    s.settle();
    assert!(s.settings_held() && !s.terminal_focused());
    s.cx.simulate_keystrokes("escape");
    s.settle();
    assert!(s.settings_closed(), "Escape closed it");
    assert!(s.terminal_focused(), "the terminal has the keyboard back");
}

#[gpui_kit::test]
fn a_click_outside_the_settings_popup_closes_it_and_one_inside_does_not(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    s.open_settings();
    s.click("settings-tab-appearance");
    s.settle();
    let view = s.settings().expect("a click inside keeps it up");
    assert_eq!(
        view.read_with(s.cx, |view, _| view.tab()),
        crate::settings::Tab::Appearance
    );
    // nothing under the dimmed layer takes the click: the sidebar's row is not selected
    let row = s.cx.debug_bounds("row:u").expect("the row is laid out").center();
    s.cx.simulate_click(row, gpui_kit::Modifiers::default());
    s.settle();
    assert!(s.settings_closed(), "a click outside closed it");
    assert_eq!(
        s.active(),
        s.session("var"),
        "the click reached nothing under the layer"
    );
    assert!(s.terminal_focused());
    // the close button closes it too
    s.open_settings();
    s.click("settings-close");
    s.settle();
    assert!(s.settings_closed() && s.terminal_focused());
}

#[gpui_kit::test]
fn opening_settings_again_while_it_is_up_focuses_the_one_popup(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    s.open_settings();
    let first = s.settings().expect("up").entity_id();
    s.click("settings-tab-agent-status");
    // the keyboard wandered off (to the terminal, as a pane's click would give it)
    let deck = s.view.read_with(s.cx, |view, _| view.deck().clone());
    s.cx.update(|window, cx| {
        let target = deck.read(cx).focus_target(cx).expect("a terminal to focus");
        window.focus(&target, cx);
    });
    assert!(!s.settings_held());
    s.open_settings();
    let again = s.settings().expect("still up");
    assert_eq!(again.entity_id(), first, "the same popup");
    assert_eq!(
        again.read_with(s.cx, |view, _| view.tab()),
        crate::settings::Tab::AgentStatus,
        "on the tab it showed"
    );
    assert!(s.settings_held(), "holding the keyboard again");
}

#[gpui_kit::test]
fn the_settings_popup_follows_the_core_gives_way_to_an_alert_and_goes_with_its_window(cx: &mut TestAppContext) {
    use agterm_core::settings_model::{GeneralChange, SettingsChange};

    let mut s = shell(cx);
    s.open_settings();
    s.state
        .core
        .change_settings(SettingsChange::General(GeneralChange::ConfirmCloseSession(false)));
    let snapshot = crate::settings::Snapshot::of(&s.state.core, s.state.applied_dark());
    s.view
        .update_in(s.cx, |view, window, cx| view.sync_settings(snapshot, window, cx));
    let shown = s
        .settings()
        .unwrap()
        .read_with(s.cx, |view, _| view.snapshot().settings.confirm_close_session);
    assert_ne!(shown, Some(true), "the popup no longer shows the seeded value");
    assert_eq!(
        shown,
        s.state.core.settings().confirm_close_session,
        "the popup shows what the core holds"
    );
    // an alert over it takes the keyboard, and gives it back once answered
    let answers = Rc::default();
    open_prompt(&mut s, &answers);
    assert!(s.alert_open() && !s.settings_held());
    s.cx.simulate_keystrokes("escape");
    s.settle();
    assert!(!s.alert_open());
    assert!(
        s.settings().is_some() && s.settings_held(),
        "the popup has the keyboard back"
    );
    // the window closing takes it down
    let popup = s.settings().unwrap().downgrade();
    let view = s.view.clone();
    view.update_in(s.cx, |view, window, cx| view.close(window, cx));
    s.cx.run_until_parked();
    assert!(popup.upgrade().is_none(), "gone with its window");
}

/// The library's widgets inside the popup paint in the chrome's colors: a picker's dropdown over a dark chrome is
/// dark, and a pick in it leaves the popup up.
#[gpui_kit::test]
fn the_settings_popups_menus_are_dark_over_a_dark_chrome(cx: &mut TestAppContext) {
    let mut s = shell(cx);
    let colors = s.paint_chrome(&themed((0x10, 0x20, 0x30), (0xe0, 0xe0, 0xe0)));
    s.open_settings();
    let panel = s.menu_panel(|s| s.click("settings-link-open-mode"));
    assert_eq!(panel, crate::chrome::color(colors.background, 1.0), "the picker's menu");
    s.cx.simulate_keystrokes("down down enter");
    s.settle();
    assert_eq!(
        s.state.core.settings().effective_link_open_mode(),
        agterm_core::persist::settings::LinkOpenMode::Overlay
    );
    assert!(s.settings().is_some(), "a pick leaves the popup up");
}
