//! The bar's keys, which search it shows, and its input landing only on the search it was typed for (ported from the
//! GTK build's `search.rs` tests); the bar in a window, its focus and counter, is in `window/tests.rs`.

use agterm_core::model::{SearchReport, StatusPane, WindowLibrary};
use agterm_core::{CoreConfig, CoreState};
use agterm_proto::AppIdentity;
use gpui_kit::Keystroke;

use super::*;
use crate::terminal::input::Source;

fn key(source: &str) -> Option<SearchInput> {
    let keystroke = Keystroke::parse(source).unwrap();
    field_key(&KeyStroke::new(&keystroke, &Source::Keystroke, false))
}

#[test]
fn return_steps_shift_return_steps_back_and_escape_closes() {
    assert_eq!(key("enter"), Some(SearchInput::Step(true)));
    assert_eq!(key("shift-enter"), Some(SearchInput::Step(false)));
    assert_eq!(key("escape"), Some(SearchInput::Close));
    assert_eq!(key("a"), None);
}

fn core(library: WindowLibrary) -> Core {
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            ..CoreState::default()
        },
    )
}

#[test]
fn the_bar_shows_the_active_sessions_open_search_unless_zoomed() {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let session = store.sessions().next().unwrap().id();
    let mut core = core(library);
    assert!(shown_search(&core, window).is_none());
    let search = &mut core
        .library_mut()
        .store_mut(window)
        .unwrap()
        .session_mut(session)
        .unwrap()
        .search;
    search.apply(StatusPane::Left, SearchReport::Started(Some("err".to_owned())));
    let (shown, state) = shown_search(&core, window).unwrap();
    assert_eq!((shown, state.needle.as_str()), (session, "err"));
    let zoom = agterm_core::zoom::SurfaceId {
        session,
        surface: agterm_core::zoom::ZoomSurface::Primary,
    };
    core.set_zoom_target(window, Some(zoom));
    assert!(shown_search(&core, window).is_none(), "hidden while zoomed");
}

#[test]
fn input_typed_for_one_search_never_lands_on_another() {
    use agterm_host::fakes::FakeSurface;

    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let first = store.sessions().next().unwrap().id();
    let workspace = store.workspaces()[0].id();
    let second = store
        .add_session(workspace, agterm_core::model::NewSession::at_cwd("/srv"))
        .unwrap();
    let mut probes = Vec::new();
    for (session, token) in [(first, "A"), (second, "B")] {
        let surface = FakeSurface::new(token);
        probes.push(surface.probe());
        let state = store.session_mut(session).unwrap();
        state.surface.replace(Box::new(surface));
        state.search.apply(StatusPane::Left, SearchReport::Started(None));
    }
    store.select_session(Some(first));
    let mut state = AppState::new(core(library));
    let typed_for_first = shown_key(&state.core, window).unwrap();
    assert_eq!(typed_for_first.0, first);
    // a queued selection change lands before the field's input
    state
        .core
        .library_mut()
        .store_mut(window)
        .unwrap()
        .select_session(Some(second));
    state.search_bar(window, typed_for_first, SearchInput::Needle("err".to_owned()));
    state.search_bar(window, typed_for_first, SearchInput::Close);
    assert!(
        probes[1].actions().is_empty(),
        "nothing reached the second session's search"
    );
    let typed_for_second = shown_key(&state.core, window).unwrap();
    state.search_bar(window, typed_for_second, SearchInput::Needle("err".to_owned()));
    assert_eq!(probes[1].actions(), ["search:err"]);
    assert!(probes[0].actions().is_empty());
}
