//! The window shell: its title (ported from the GTK build's `window.rs` tests), the state `window.list` reads, and the
//! shell over a deck in GPUI's test window.

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
    let (view, cx) = cx.add_window_view(move |root, cx| {
        let deck = cx.new(|_| Deck::new(window, env));
        WindowView::new(window, deck, poster, 0, root, cx)
    });
    cx.simulate_resize(size(px(640.), px(480.)));
    let _ = view.update_in(cx, |view, window, cx| view.sync(&mut core, window, cx));
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
            [Layer::Ask, Layer::Picker],
            "back to front, whatever the attach order"
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
    view.update_in(cx, |view, root, _| view.close(root));
    assert_eq!(crate::keymap_dispatch::window_of(origin), None);
    assert!(!crate::keymap_dispatch::holds_keyboard(Some(handle)));
}

/// A layer view that draws nothing.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
