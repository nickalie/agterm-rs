//! The dashboard on real pages (the GTK build's `the_dashboard_lays_member_panes_out_in_place` walk): every member
//! pane's own terminal drawn in its cell and resized to it, the rest hidden; the grid's keyboard walking the highlight,
//! entering a cell and closing; a click flashing then entering; the font modes applied transiently and put back
//! without touching the session's font; a picker over the grid keeping the keyboard. And the terminal zoom it
//! replaces: one terminal filling the page, the split back at its ratio once it ends.

use agterm_core::dashboard::DashboardMember;

use super::*;

fn open_dashboard(h: &mut Harness<'_>, edit: impl FnOnce(&mut Args)) {
    let mut args = Args::default();
    edit(&mut args);
    let response = h.state.core.dispatch(&Request {
        cmd: Command::Dashboard,
        target: None,
        args: Some(args),
    });
    assert!(response.ok, "{response:?}");
    h.state.settle();
    h.turn();
}

/// Whether the grid's focus holds the window's keyboard.
fn grid_focused(h: &mut Harness<'_>) -> bool {
    let deck = h.deck.clone();
    h.cx.update(|window, cx| deck.read(cx).grid_focus().is_some_and(|focus| focus.is_focused(window)))
}

/// Two started sessions, the second split: its ids and the three pane tokens (first, second left, second right).
fn two_with_a_split(h: &mut Harness<'_>, ids: &[Uuid]) -> [String; 3] {
    let (first, second) = (ids[0], ids[1]);
    h.until("both primaries to start", |h| {
        h.realized(first, StatusPane::Left) && h.realized(second, StatusPane::Left)
    });
    h.dispatch(Command::Session(SessionCmd::Split), second, |_| {});
    h.until("the split pane to start", |h| h.realized(second, StatusPane::Right));
    [
        h.token(first, StatusPane::Left),
        h.token(second, StatusPane::Left),
        h.token(second, StatusPane::Right),
    ]
}

#[gpui_kit::test]
fn the_grid_hosts_each_member_terminal_in_its_cell_and_gives_it_back(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let [first, left, right] = two_with_a_split(&mut h, &ids);
    let (one, two) = (ids[0], ids[1]);
    h.until("the first page's full grid", |h| h.cols(one, &first) == 79);

    let targets = vec![uuid_string(one), uuid_string(two)];
    open_dashboard(&mut h, |args| args.targets = Some(targets));
    h.deck.read_with(h.cx, |deck, _| {
        assert!(deck.grid_open());
        assert_eq!(deck.shown(), None, "no page shows under the grid");
        assert_eq!(
            deck.grid_terminals(),
            vec![
                Some((one, first.clone())),
                Some((two, left.clone())),
                Some((two, right.clone()))
            ]
        );
    });
    for (session, token) in [(one, &first), (two, &left), (two, &right)] {
        assert!(h.shown(session, token), "a member terminal draws in its cell");
    }
    assert!(
        !h.shown(ids[2], &h.token(ids[2], StatusPane::Left)),
        "a non-member stays hidden"
    );
    assert!(h.cx.debug_bounds("dashboard").is_some());
    assert!(h.cx.debug_bounds("dashboard-cell-2").is_some(), "three cells");
    assert!(h.cx.debug_bounds("dashboard-highlighted").is_some());
    // three cells make a 2x2 grid: (803 - 3 * 12) / 2 = 383 px, 37 columns
    h.until("the cell's grid", |h| h.cols(one, &first) == 37);
    assert!(grid_focused(&mut h), "the grid holds the keyboard");
    assert!(!h.focused(one, &first));

    // Escape closes it: the terminals go back to their pages at the page's size, the shown pane takes the keyboard
    h.cx.simulate_keystrokes("escape");
    h.turn();
    assert!(h.state.core.dashboard(window).is_none());
    h.turn();
    h.deck.read_with(h.cx, |deck, _| assert!(!deck.grid_open()));
    let selected = h
        .state
        .core
        .library()
        .store(window)
        .unwrap()
        .selected_session_id()
        .unwrap();
    assert_eq!(selected, one);
    h.until("the page's grid again", |h| h.cols(one, &first) == 79);
    assert!(h.focused(one, &first), "the shown pane has the keyboard back");
    assert!(!h.shown(two, &left), "the other member's page is hidden again");
}

#[gpui_kit::test]
fn the_arrows_walk_the_highlight_and_enter_selects_that_pane(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let [_, _, right] = two_with_a_split(&mut h, &ids);
    let targets = vec![uuid_string(ids[0]), uuid_string(ids[1])];
    open_dashboard(&mut h, |args| args.targets = Some(targets));
    let highlighted = |h: &Harness<'_>| {
        h.state
            .core
            .dashboard(window)
            .and_then(agterm_core::dashboard::DashboardController::highlighted)
    };
    assert_eq!(highlighted(&h), Some(DashboardMember::primary(ids[0])));
    // a letter is swallowed: nothing types, nothing moves
    h.cx.simulate_keystrokes("x right");
    h.turn();
    assert_eq!(highlighted(&h), Some(DashboardMember::primary(ids[1])));
    h.cx.simulate_keystrokes("left down");
    h.turn();
    assert_eq!(highlighted(&h), Some(DashboardMember::split(ids[1])));
    h.cx.simulate_keystrokes("enter");
    h.turn();
    h.turn();
    assert!(h.state.core.dashboard(window).is_none(), "Enter enters and closes");
    let store = h.state.core.library().store(window).unwrap();
    assert_eq!(store.selected_session_id(), Some(ids[1]));
    assert!(h.session(ids[1]).split.focused, "the entered pane is the focused one");
    h.until("the entered pane to take the keyboard", |h| h.focused(ids[1], &right));
}

#[gpui_kit::test]
fn a_click_rings_its_cell_then_enters_it(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    h.until("the panes to start", |h| {
        h.realized(ids[0], StatusPane::Left) && h.realized(ids[1], StatusPane::Left)
    });
    let targets = vec![uuid_string(ids[0]), uuid_string(ids[1])];
    open_dashboard(&mut h, |args| args.targets = Some(targets));
    let at = h.cx.debug_bounds("dashboard-cell-1").expect("the second cell").center();
    h.cx.simulate_click(at, gpui_kit::Modifiers::default());
    // the test loop runs a delayed task at once: the ring, then the enter
    let tasks = std::mem::take(&mut *h.posted.0.lock().unwrap());
    assert_eq!(tasks.len(), 2, "the highlight and the delayed enter");
    let mut tasks = tasks.into_iter();
    (tasks.next().unwrap())(&mut h.state);
    assert_eq!(
        h.state
            .core
            .dashboard(window)
            .and_then(agterm_core::dashboard::DashboardController::highlighted),
        Some(DashboardMember::primary(ids[1])),
        "the clicked cell rings first"
    );
    (tasks.next().unwrap())(&mut h.state);
    assert!(h.state.core.dashboard(window).is_none(), "then the click enters it");
    assert_eq!(
        h.state.core.library().store(window).unwrap().selected_session_id(),
        Some(ids[1])
    );
}

#[gpui_kit::test]
fn a_fixed_font_is_transient_and_never_the_sessions(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    h.until("the panes to start", |h| {
        h.realized(ids[0], StatusPane::Left) && h.realized(ids[1], StatusPane::Left)
    });
    let token = h.token(ids[0], StatusPane::Left);
    let shared = h.shared(ids[0], &token);
    let before = shared.font_points();
    let stored = h.session(ids[0]).font_size;
    let targets = vec![uuid_string(ids[0]), uuid_string(ids[1])];
    open_dashboard(&mut h, |args| {
        args.targets = Some(targets);
        args.font_size = Some(9.0);
    });
    assert!((shared.font_points() - 9.0).abs() < f32::EPSILON, "the grid's size");
    assert_eq!(shared.font_override(), Some(9.0));
    h.deck.read_with(h.cx, |deck, _| {
        assert_eq!(deck.font_overrides().applied(&token), Some(9.0));
    });
    h.turn();
    h.turn();
    assert_eq!(
        h.session(ids[0]).font_size,
        stored,
        "the grid's size is not the session's"
    );

    let response = h.state.core.dispatch(&Request {
        cmd: Command::Dashboard,
        target: None,
        args: Some(Args {
            close: Some(true),
            ..Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    h.state.settle();
    h.turn();
    h.turn();
    assert!((shared.font_points() - before).abs() < f32::EPSILON, "put back");
    assert_eq!(shared.font_override(), None);
    assert_eq!(h.session(ids[0]).font_size, stored, "nor is the revert");
    // a later zoom is the pane's own again
    assert!(shared.set_font_points(before + 2.0));
    h.until("the zoom's report", |h| {
        h.session(ids[0]).font_size == Some(f64::from(before + 2.0))
    });
}

fn zoom(h: &mut Harness<'_>, target: String, mode: &str) {
    let response = h.state.core.dispatch(&Request {
        cmd: Command::Surface(agterm_proto::command::SurfaceCmd::Zoom),
        target: Some(target),
        args: Some(Args {
            mode: Some(mode.to_owned()),
            ..Args::default()
        }),
    });
    assert!(response.ok, "{response:?}");
    h.state.settle();
    h.turn();
}

#[gpui_kit::test]
fn a_zoom_fills_the_page_with_one_terminal_and_its_end_lays_the_split_out_again(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let [_, left, right] = two_with_a_split(&mut h, &ids);
    let session = ids[1];
    h.store().select_session(Some(session));
    h.state.settle();
    h.turn();
    h.until("the even split", |h| {
        h.cols(session, &left) == 39 && h.cols(session, &right) == 39
    });

    zoom(&mut h, format!("surface:{}:right", uuid_string(session)), "show");
    assert!(h.state.core.zoom_target(window).is_some());
    assert!(
        h.shown(session, &right) && !h.shown(session, &left),
        "the zoomed terminal alone"
    );
    h.until("the zoomed terminal's full grid", |h| h.cols(session, &right) == 79);
    assert!(
        h.focused(session, &right),
        "the zoom keeps the keyboard on its terminal"
    );

    zoom(&mut h, format!("surface:{}:right", uuid_string(session)), "hide");
    assert!(h.state.core.zoom_target(window).is_none());
    assert!(h.shown(session, &left) && h.shown(session, &right), "the split again");
    h.until("the split at its ratio", |h| {
        h.cols(session, &left) == 39 && h.cols(session, &right) == 39
    });
    assert!(h.focused(session, &right), "the keyboard where it was");

    // the dashboard and the zoom replace each other
    zoom(&mut h, format!("surface:{}:left", uuid_string(session)), "show");
    open_dashboard(&mut h, |args| args.mru = Some(true));
    assert!(h.state.core.zoom_target(window).is_none(), "the grid ends the zoom");
    h.deck.read_with(h.cx, |deck, _| assert!(deck.grid_open()));
}

#[gpui_kit::test]
fn a_picker_over_the_grid_keeps_the_keyboard_until_it_is_answered(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    h.until("the pane to start", |h| h.realized(ids[0], StatusPane::Left));
    open_dashboard(&mut h, |args| args.mru = Some(true));
    assert!(grid_focused(&mut h));
    // a modal elsewhere (the window's picker) holds the keyboard: every reconcile leaves it alone
    let elsewhere = h.cx.update(|window, cx| {
        let handle = cx.focus_handle();
        window.focus(&handle, cx);
        handle
    });
    let (deck, state) = (h.deck.clone(), &mut h.state);
    h.cx.update(|window, cx| {
        let _ = deck.update(cx, |deck, cx| deck.reconcile(&mut state.core, true, window, cx));
    });
    assert!(h.cx.update(|window, _| elsewhere.is_focused(window)));
    // once it is gone the grid takes the keyboard back
    h.turn();
    assert!(grid_focused(&mut h));
}
