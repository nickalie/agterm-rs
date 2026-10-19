//! The sidebar in GPUI's test window over a real core: the rows following the model, clicks selecting (plain, Ctrl,
//! Shift) and the right-click narrowing, a workspace row folding after the double-click interval and through its
//! disclosure, inline rename committed and cancelled, drag reorder of sessions and workspaces, spring-loading, the
//! hover "+", the footer's toggles, the close confirmation, the divider, and the status glyph's states. The sidebar's
//! posts land on a recording loop the test runs against the app state, then the sidebar syncs as the window would.

use std::sync::{Arc, Mutex};

use agterm_core::model::{AgentIndicator, AgentStatus, NewSession, SidebarMode, WindowLibrary};
use agterm_core::persist::AppSettings;
use agterm_core::{CoreConfig, CoreState};
use agterm_host::{MainLoop, MainLoopClosed, MainTask};
use agterm_proto::AppIdentity;
use gpui_kit::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, point, px, size,
};

use super::*;
use crate::terminal::TerminalConfig;

/// The main loop the sidebar posts to: tasks wait for the test to run them; timers are not exercised.
#[derive(Default)]
struct Posted(Mutex<Vec<MainTask<AppState>>>);

impl MainLoop<AppState> for Posted {
    fn post(&self, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.0.lock().unwrap().push(task);
        Ok(())
    }

    fn post_after(&self, _: Duration, _: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        Ok(())
    }
}

/// A window's sidebar over a real core.
struct Harness {
    state: AppState,
    posted: Arc<Posted>,
    view: Entity<Sidebar>,
    window: Uuid,
}

/// Workspace "workspace 1" with sessions "u" and "srv" (selected), workspace "two" with "var".
fn library() -> (WindowLibrary, Uuid) {
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let one = store.workspaces()[0].id();
    let srv = store.add_session(one, NewSession::at_cwd("/srv")).unwrap();
    let two = store.add_workspace("two");
    store.add_session(two, NewSession::at_cwd("/var")).unwrap();
    store.select_session(Some(srv));
    (library, window)
}

fn harness(
    cx: &mut TestAppContext,
    library: WindowLibrary,
    settings: AppSettings,
) -> (Harness, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let window = library.active_window_id().unwrap();
    let core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            ..CoreState::default()
        },
    );
    let posted = Arc::new(Posted::default());
    let weak_loop: Poster = Arc::downgrade(&posted) as Poster;
    let mut state = AppState::new(core);
    state.attach(weak_loop.clone());
    let (view, cx) = cx.add_window_view(move |_, _| Sidebar::new(window, weak_loop));
    cx.simulate_resize(size(px(640.), px(480.)));
    let mut harness = Harness {
        state,
        posted,
        view,
        window,
    };
    harness.settle(cx);
    (harness, cx)
}

impl Harness {
    /// Runs what the sidebar posted against the app state and syncs the sidebar after each round, as the window's
    /// landing does, until nothing more is posted.
    fn settle(&mut self, cx: &mut VisualTestContext) {
        for _ in 0..8 {
            cx.run_until_parked();
            let tasks: Vec<MainTask<AppState>> = std::mem::take(&mut *self.posted.0.lock().unwrap());
            for task in tasks {
                task(&mut self.state);
            }
            let core = &mut self.state.core;
            self.view.update(cx, |sidebar, cx| sidebar.sync(core, cx));
            cx.run_until_parked();
            if self.posted.0.lock().unwrap().is_empty() {
                return;
            }
        }
    }

    fn store(&self) -> &agterm_core::model::AppStore {
        self.state.core.library().store(self.window).unwrap()
    }

    fn store_mut(&mut self) -> &mut agterm_core::model::AppStore {
        self.state.core.library_mut().store_mut(self.window).unwrap()
    }

    /// The rows as `w:`/`s:` labels, `*` marking a selected session.
    fn names(&self, cx: &mut VisualTestContext) -> Vec<String> {
        self.view.read_with(cx, |sidebar, _| {
            sidebar
                .rows()
                .iter()
                .map(|row| {
                    let kind = if row.is_workspace() { "w" } else { "s" };
                    let mark = if matches!(row.kind, RowKind::Session { selected: true, .. }) {
                        "*"
                    } else {
                        ""
                    };
                    format!("{kind}:{}{mark}", row.label)
                })
                .collect()
        })
    }

    fn id_of(&self, label: &str, cx: &mut VisualTestContext) -> Uuid {
        self.view.read_with(cx, |sidebar, _| {
            sidebar
                .rows()
                .iter()
                .find(|row| row.label == label)
                .map(|row| row.id)
                .unwrap()
        })
    }
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

/// Drags with the left button from `from` to `to`, in steps, and releases there.
fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for step in 1..=4_u8 {
        let t = f32::from(step) / 4.0;
        let at = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::default());
    }
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
}

// MARK: - the pure parts

fn session_row(id: Uuid, selected: bool) -> SidebarRow {
    SidebarRow {
        id,
        kind: RowKind::Session {
            workspace: Uuid::nil(),
            nested: true,
            selected,
        },
        label: String::new(),
        badge: 0,
        icon: crate::row_icon::Glyph::Session {
            split: None,
            flagged: false,
        },
        status: None,
        icon_tooltip: None,
    }
}

#[test]
fn a_click_selects_alone_ctrl_toggles_and_shift_extends_from_the_anchor() {
    let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    let mut rows: Vec<SidebarRow> = ids.iter().map(|id| session_row(*id, false)).collect();
    // a workspace row between them takes no part
    rows.insert(
        2,
        SidebarRow {
            kind: RowKind::Workspace { expanded: true },
            ..session_row(Uuid::new_v4(), false)
        },
    );
    let plain = Modifiers::default();
    assert_eq!(click_selection(&rows, &[ids[0]], Some(ids[0]), ids[2], plain), [ids[2]]);
    assert_eq!(
        click_selection(&rows, &[ids[2]], Some(ids[2]), ids[0], ctrl()),
        [ids[0], ids[2]],
        "Ctrl adds, in visible order"
    );
    assert_eq!(
        click_selection(&rows, &[ids[0], ids[2]], Some(ids[0]), ids[0], ctrl()),
        [ids[2]]
    );
    assert_eq!(
        click_selection(&rows, &[ids[1]], Some(ids[1]), ids[3], shift()),
        [ids[1], ids[2], ids[3]]
    );
    assert_eq!(
        click_selection(&rows, &[ids[3]], Some(ids[3]), ids[0], shift()),
        ids[..4]
    );
    assert_eq!(
        click_selection(&rows, &[], None, ids[1], shift()),
        [ids[1]],
        "no anchor: the row alone"
    );
    let both = Modifiers {
        control: true,
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(
        click_selection(&rows, &[ids[3]], Some(ids[0]), ids[1], both),
        [ids[0], ids[1], ids[3]],
        "Ctrl+Shift adds the range"
    );
}

#[test]
fn a_new_spring_load_folds_the_workspace_an_earlier_one_opened() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let mut drag = DragState::default();
    assert_eq!(drag.spring_opened(a), None);
    assert_eq!(drag.spring_opened(a), None, "the same workspace stays open");
    assert_eq!(drag.spring_opened(b), Some(a));
    assert_eq!(drag.spring_open, Some(b), "the drag now folds only b back");
}

#[test]
fn confirmations_use_upstreams_words() {
    let mut context = SidebarContext {
        close_grace: true,
        ..SidebarContext::default()
    };
    let id = Uuid::new_v4();
    context.menu.sessions.insert(
        id,
        menu::SessionFacts {
            name: "build".into(),
            workspace: Uuid::nil(),
            flagged: false,
            has_status: false,
            cwd: "/".into(),
        },
    );
    assert_eq!(
        close_confirmation(&context, &[id]),
        (
            "Close “build”?".to_owned(),
            "The session will close after a short undo window.".to_owned()
        )
    );
    context.close_grace = false;
    assert_eq!(
        close_confirmation(&context, &[id, Uuid::nil()]),
        (
            "Close 2 Sessions?".to_owned(),
            "The sessions will close immediately and can be reopened from File > Open Recent.".to_owned()
        )
    );
    assert_eq!(
        delete_confirmation(1, true),
        "This closes its session after a short undo window."
    );
    assert_eq!(
        delete_confirmation(3, false),
        "This closes 3 sessions immediately. You can reopen it from File > Open Recent."
    );
}

#[test]
fn the_look_paints_the_chromes_colors_in_the_font_setting() {
    let plain = Look::of(crate::chrome::default_colors(), &AppSettings::default());
    assert_eq!(plain, Look::default());
    assert!((plain.font_size - look::DEFAULT_FONT_SIZE).abs() < f32::EPSILON);
    assert_eq!(plain.colors.sidebar, plain.colors.background, "the neutral tint");
    let settings = AppSettings {
        sidebar_background_shift: Some(10),
        sidebar_font_size: Some(42.0),
        ..AppSettings::default()
    };
    let colors = crate::chrome::colors(&TerminalConfig::default(), &settings);
    let tinted = Look::of(colors, &settings);
    assert_ne!(tinted.colors.sidebar, tinted.colors.background, "a darker wash");
    assert!((tinted.font_size - 20.0).abs() < f32::EPSILON, "clamped to 20");
    assert!(tinted.row_height() > plain.row_height());
    // the divider, the pill and the selected text are the chrome's, as the title-bar hairline is
    assert_eq!(tinted.separator(), crate::chrome::separator(&colors));
    for key in [true, false] {
        let (pill, text) = crate::chrome::selected_row(&colors, key);
        assert_eq!((tinted.pill(key), tinted.text(true)), (pill, text));
    }
}

#[gpui_kit::test]
fn the_sidebar_paints_the_chrome_the_window_shows(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx, library().0, AppSettings::default());
    let shown = h.view.read_with(cx, |sidebar, _| sidebar.look);
    assert_eq!(
        shown.colors,
        crate::chrome::default_colors(),
        "the default before the first chrome sync"
    );
    let mut config = TerminalConfig::default();
    config.theme.background = agterm_term::Rgb::new(0x10, 0x20, 0x30);
    config.theme.foreground = agterm_term::Rgb::new(0xe0, 0xe0, 0xe0);
    let settings = AppSettings {
        sidebar_background_shift: Some(10),
        ..AppSettings::default()
    };
    let chrome = crate::chrome::colors(&config, &settings);
    cx.update(|_, cx| cx.set_global(crate::chrome::Chrome(chrome)));
    h.settle(cx);
    let shown = h.view.read_with(cx, |sidebar, _| sidebar.look);
    assert_eq!(
        shown.colors, chrome,
        "the tint, the divider and the pill follow the chrome"
    );
}

// MARK: - in the window

#[gpui_kit::test]
fn the_rows_follow_the_model_and_clicks_select(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    assert_eq!(h.names(cx), ["w:workspace 1", "s:u", "s:srv*", "w:two", "s:var"]);
    let var = h.id_of("var", cx);
    let at = bounds(cx, "row:var").center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.store().selected_session_id(), Some(var));
    assert_eq!(h.names(cx)[4], "s:var*");

    // Ctrl adds a row, Shift extends from the anchor
    let at = bounds(cx, "row:u").center();
    cx.simulate_click(at, ctrl());
    h.settle(cx);
    assert_eq!(h.names(cx), ["w:workspace 1", "s:u*", "s:srv", "w:two", "s:var*"]);
    let at = bounds(cx, "row:srv").center();
    cx.simulate_click(at, shift());
    h.settle(cx);
    assert_eq!(h.names(cx), ["w:workspace 1", "s:u*", "s:srv*", "w:two", "s:var"]);
    assert_eq!(h.store().selected_session_id(), Some(h.id_of("srv", cx)));

    // a right-click outside the selection narrows it to the row
    let at = bounds(cx, "row:var").center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    let at = bounds(cx, "row:var").center();
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.names(cx), ["w:workspace 1", "s:u", "s:srv", "w:two", "s:var*"]);

    // a rename in the model updates the row in place
    let srv = h.id_of("srv", cx);
    h.store_mut().rename_session(srv, "build");
    h.settle(cx);
    assert_eq!(h.names(cx)[2], "s:build");
}

#[gpui_kit::test]
fn a_workspace_row_folds_after_the_double_click_interval_and_through_its_disclosure(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let two = h.id_of("two", cx);
    let at = bounds(cx, "row:two").center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.names(cx).len(), 5, "nothing before the interval ends");
    cx.executor().advance_clock(DOUBLE_CLICK);
    h.settle(cx);
    assert_eq!(h.names(cx), ["w:workspace 1", "s:u", "s:srv*", "w:two"]);
    assert!(!h.store().workspace(two).unwrap().is_expanded, "a user fold persists");

    // the disclosure opens it at once
    let disclosure: &'static str = Box::leak(format!("disclosure:{two}").into_boxed_str());
    let at = bounds(cx, disclosure).center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.names(cx).len(), 5);
    assert!(h.store().workspace(two).unwrap().is_expanded);

    // with the whole-row setting off, a row click does nothing
    h.state
        .core
        .update_settings(|settings| settings.workspace_row_click_expands = Some(false));
    h.settle(cx);
    let at = bounds(cx, "row:two").center();
    cx.simulate_click(at, Modifiers::default());
    cx.executor().advance_clock(DOUBLE_CLICK);
    h.settle(cx);
    assert_eq!(h.names(cx).len(), 5);
}

#[gpui_kit::test]
fn a_double_click_renames_enter_commits_and_escape_cancels(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let srv = h.id_of("srv", cx);
    let two = h.id_of("two", cx);
    h.view.update_in(cx, |sidebar, window, cx| {
        sidebar.row_clicked(two, 1, Modifiers::default(), window, cx);
        sidebar.row_clicked(two, 2, Modifiers::default(), window, cx);
    });
    cx.executor().advance_clock(DOUBLE_CLICK);
    h.settle(cx);
    assert!(
        h.store().workspace(two).unwrap().is_expanded,
        "the double-click cancelled the toggle"
    );
    assert_eq!(h.view.read_with(cx, |sidebar, _| sidebar.editing()), Some(two));
    assert!(cx.debug_bounds("edit-field").is_some());
    cx.simulate_input("ops");
    cx.simulate_keystrokes("enter");
    h.settle(cx);
    assert_eq!(h.store().workspace(two).unwrap().name, "ops");
    assert_eq!(h.view.read_with(cx, |sidebar, _| sidebar.editing()), None);

    h.view
        .update_in(cx, |sidebar, window, cx| sidebar.begin_rename(srv, window, cx));
    h.settle(cx);
    cx.simulate_input("gone");
    cx.simulate_keystrokes("escape");
    h.settle(cx);
    assert_eq!(h.store().session(srv).unwrap().display_name(), "srv");
    assert_eq!(h.view.read_with(cx, |sidebar, _| sidebar.editing()), None);
}

#[gpui_kit::test]
fn a_press_outside_the_rename_field_commits_it(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let u = h.id_of("u", cx);
    let two = h.id_of("two", cx);
    h.view
        .update_in(cx, |sidebar, window, cx| sidebar.begin_rename(u, window, cx));
    h.settle(cx);
    cx.simulate_input("build");
    // the disclosure takes no focus, and the fold it makes reshapes the list
    let disclosure: &'static str = Box::leak(format!("disclosure:{two}").into_boxed_str());
    let at = bounds(cx, disclosure).center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.store().session(u).unwrap().display_name(), "build", "committed");
    assert_eq!(h.view.read_with(cx, |sidebar, _| sidebar.editing()), None);
    assert!(
        !h.store().workspace(two).unwrap().is_expanded,
        "the click still folded two"
    );

    // another workspace row's click commits too
    h.view
        .update_in(cx, |sidebar, window, cx| sidebar.begin_rename(two, window, cx));
    h.settle(cx);
    cx.simulate_input("ops");
    let at = bounds(cx, "row:workspace 1").center();
    cx.simulate_click(at, Modifiers::default());
    cx.executor().advance_clock(DOUBLE_CLICK);
    h.settle(cx);
    assert_eq!(h.store().workspace(two).unwrap().name, "ops");
}

#[gpui_kit::test]
fn shift_extends_from_a_selection_made_elsewhere(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let at = bounds(cx, "row:u").center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    // the keymap, ngtermctl or auto-follow moves the selection to var
    let var = h.id_of("var", cx);
    h.store_mut().select_session(Some(var));
    h.settle(cx);
    let at = bounds(cx, "row:srv").center();
    cx.simulate_click(at, shift());
    h.settle(cx);
    assert_eq!(
        h.names(cx),
        ["w:workspace 1", "s:u", "s:srv*", "w:two", "s:var*"],
        "from var, not from u"
    );
    // the sidebar's own Shift-click keeps the anchor
    let at = bounds(cx, "row:u").center();
    cx.simulate_click(at, shift());
    h.settle(cx);
    assert_eq!(h.names(cx), ["w:workspace 1", "s:u*", "s:srv*", "w:two", "s:var*"]);
}

#[gpui_kit::test]
fn a_row_menu_holds_the_keyboard_until_it_closes(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    assert!(!h.view.read_with(cx, |sidebar, _| sidebar.menu_open()));
    let at = bounds(cx, "row:var").center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    assert!(h.view.read_with(cx, |sidebar, _| sidebar.menu_open()), "open");
    h.settle(cx);
    assert!(
        h.view.read_with(cx, |sidebar, _| sidebar.menu_open()),
        "a sync keeps it"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!h.view.read_with(cx, |sidebar, _| sidebar.menu_open()), "closed");
    assert!(
        !h.posted.0.lock().unwrap().is_empty(),
        "the close asks for a sync, which lands the focus the deck held"
    );
}

fn right_click(cx: &mut VisualTestContext, row: &'static str) {
    let at = bounds(cx, row).center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

fn menu_open(h: &Harness, cx: &mut VisualTestContext) -> bool {
    h.view.read_with(cx, |sidebar, _| sidebar.menu_open())
}

#[gpui_kit::test]
fn a_row_menu_dropped_without_a_dismiss_stops_holding_the_keyboard(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    // its row goes (closed through ngtermctl, its shell exited) while the menu is open
    right_click(cx, "row:var");
    assert!(menu_open(&h, cx), "open");
    let var = h.id_of("var", cx);
    h.store_mut().close_session(var);
    let core = &mut h.state.core;
    h.view.update(cx, |sidebar, cx| sidebar.sync(core, cx));
    cx.run_until_parked();
    assert!(!menu_open(&h, cx), "the dropped menu holds nothing");
    assert!(
        !h.posted.0.lock().unwrap().is_empty(),
        "the drop asks for a sync, which lands the focus the deck held"
    );
    h.settle(cx);

    // the sidebar hides while the menu is open
    right_click(cx, "row:u");
    assert!(menu_open(&h, cx), "open again");
    h.store_mut().set_sidebar_visible(false);
    h.settle(cx);
    assert!(!menu_open(&h, cx), "a hidden sidebar's menu holds nothing");

    // a second menu replaces the first: the first's going leaves the second holding the keyboard
    h.store_mut().set_sidebar_visible(true);
    h.settle(cx);
    right_click(cx, "row:u");
    right_click(cx, "row:srv");
    h.settle(cx);
    assert!(menu_open(&h, cx), "the second menu holds it");
    cx.simulate_keystrokes("escape");
    h.settle(cx);
    assert!(!menu_open(&h, cx), "closed");
}

#[gpui_kit::test]
fn sessions_and_workspaces_reorder_by_drag(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    // var onto the upper half of u: before it, in workspace 1
    let u = bounds(cx, "row:u");
    let target = point(u.center().x, u.origin.y + px(2.));
    let at = bounds(cx, "row:var").center();
    drag(cx, at, target);
    h.settle(cx);
    assert_eq!(h.names(cx), ["w:workspace 1", "s:var", "s:u", "s:srv*", "w:two"]);

    // workspace two above workspace 1
    let first = bounds(cx, "row:workspace 1");
    let at = bounds(cx, "row:two").center();
    drag(cx, at, point(first.center().x, first.origin.y + px(2.)));
    h.settle(cx);
    assert_eq!(h.names(cx)[0], "w:two");
    assert_eq!(h.store().workspaces()[0].name, "two");
    assert_eq!(
        h.view.read_with(cx, |sidebar, _| sidebar.drag.highlight),
        None,
        "the highlight goes"
    );
}

#[gpui_kit::test]
fn a_hovering_session_drag_spring_loads_a_folded_workspace_and_folds_it_back_unlanded(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let two = h.id_of("two", cx);
    h.view.update(cx, |sidebar, _| sidebar.toggle(two));
    h.settle(cx);
    assert_eq!(h.names(cx).len(), 4);
    let from = bounds(cx, "row:u").center();
    let over = bounds(cx, "row:two").center();
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(point(from.x, from.y + px(4.)), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(over, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(over, MouseButton::Left, Modifiers::default());
    cx.executor().advance_clock(SPRING_DELAY);
    h.settle(cx);
    assert_eq!(h.names(cx).len(), 5, "two opened for the drag");
    assert!(!h.store().workspace(two).unwrap().is_expanded, "for view only");
    // released outside the list: nothing lands, two folds back
    cx.simulate_mouse_move(point(px(630.), px(470.)), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(point(px(630.), px(470.)), MouseButton::Left, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.names(cx).len(), 4);
}

#[gpui_kit::test]
fn the_hover_plus_adds_a_session_and_the_footer_toggles_the_flagged_view(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let two = h.id_of("two", cx);
    let plus: &'static str = Box::leak(format!("workspace-add-session:{two}").into_boxed_str());
    assert!(cx.debug_bounds(plus).is_none(), "hidden until hovered");
    let at = bounds(cx, "row:two").center();
    cx.simulate_mouse_move(at, None, Modifiers::default());
    h.settle(cx);
    let at = bounds(cx, plus).center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.store().workspace(two).unwrap().sessions.len(), 2);

    // nothing flagged: the toggle is muted and does nothing
    let at = bounds(cx, "flagged-view-toggle").center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.store().sidebar().mode, SidebarMode::Tree);
    let u = h.id_of("u", cx);
    h.store_mut().set_flag(true, u);
    h.settle(cx);
    let at = bounds(cx, "flagged-view-toggle").center();
    cx.simulate_click(at, Modifiers::default());
    h.settle(cx);
    assert_eq!(h.store().sidebar().mode, SidebarMode::Flagged);
    assert_eq!(h.names(cx), ["s:u : workspace 1*"]);
    // unflagged, the flagged view shows its hint
    h.store_mut().set_flag(false, u);
    h.settle(cx);
    assert!(h.view.read_with(cx, |sidebar, _| sidebar.context.empty_flagged));
}

#[gpui_kit::test]
fn a_confirmed_close_closes_and_a_cancelled_one_does_not(cx: &mut TestAppContext) {
    let (library, _) = library();
    let settings = AppSettings {
        confirm_close_session: Some(true),
        close_grace_undo_enabled: Some(false),
        ..AppSettings::default()
    };
    let (mut h, cx) = harness(cx, library, settings);
    let var = h.id_of("var", cx);
    h.view
        .update_in(cx, |sidebar, window, cx| sidebar.close_sessions(vec![var], window, cx));
    cx.run_until_parked();
    assert!(cx.update(|window, cx| crate::modal::answer_front_alert(window, "Cancel", cx)));
    h.settle(cx);
    assert!(h.store().session(var).is_some());
    h.view
        .update_in(cx, |sidebar, window, cx| sidebar.close_sessions(vec![var], window, cx));
    cx.run_until_parked();
    assert!(cx.update(|window, cx| crate::modal::answer_front_alert(window, "Close", cx)));
    h.settle(cx);
    assert!(h.store().session(var).is_none());
}

#[gpui_kit::test]
fn the_divider_resizes_and_a_hidden_or_covered_sidebar_steps_aside(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let before = h.store().sidebar().width;
    let handle = bounds(cx, "sidebar-divider").center();
    drag(cx, handle, point(handle.x + px(60.), handle.y));
    h.settle(cx);
    let after = h.store().sidebar().width;
    assert!((after - (before + 60.0)).abs() <= 1.0, "{before} → {after}");
    let at = bounds(cx, "sidebar-divider").center();
    drag(cx, at, point(px(1.), handle.y));
    h.settle(cx);
    assert!(
        (h.store().sidebar().width - SIDEBAR_WIDTH_MIN).abs() < f64::EPSILON,
        "clamped at the floor"
    );

    h.store_mut().set_sidebar_visible(false);
    h.settle(cx);
    assert!(!h.view.read_with(cx, |sidebar, _| sidebar.is_shown()));
    assert!(cx.debug_bounds("sidebar-divider").is_none());

    // the dashboard covers it without hiding it
    h.store_mut().set_sidebar_visible(true);
    h.settle(cx);
    assert!(h.view.read_with(cx, |sidebar, _| sidebar.is_shown()));
    let window = h.window;
    h.state.core.toggle_dashboard(window);
    assert!(h.state.core.dashboard(window).is_some(), "the dashboard opened");
    h.settle(cx);
    assert!(h.store().sidebar().visible, "still visible in the model");
    assert!(!h.view.read_with(cx, |sidebar, _| sidebar.is_shown()), "covered");
    assert!(cx.debug_bounds("sidebar-divider").is_none());
    h.state.core.toggle_dashboard(window);
    h.settle(cx);
    assert!(
        h.view.read_with(cx, |sidebar, _| sidebar.is_shown()),
        "back once uncovered"
    );
}

#[gpui_kit::test]
fn status_glyphs_show_for_a_status_and_go_when_idle(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    assert!(cx.debug_bounds("agent-status").is_none(), "idle draws nothing");
    let srv = h.id_of("srv", cx);
    h.store_mut().set_agent_indicator(
        AgentIndicator {
            status: AgentStatus::Blocked,
            blink: true,
            ..AgentIndicator::default()
        },
        srv,
    );
    h.settle(cx);
    let row = h.view.read_with(cx, |sidebar, _| sidebar.rows()[2].clone());
    assert_eq!(
        row.status.as_ref().map(|look| (look.status, look.blink)),
        Some((AgentStatus::Blocked, true))
    );
    let glyph = bounds(cx, "agent-status");
    let row_bounds = bounds(cx, "row:srv");
    assert!(row_bounds.contains(&glyph.center()), "the glyph sits in its row");
    assert!(cx.debug_bounds("agent-status-blinking").is_some(), "it blinks");
    // the desktop's reduced motion stills it
    cx.update(|_, cx| cx.set_global(crate::status_glyph::Motion { animations: false }));
    h.settle(cx);
    assert!(cx.debug_bounds("agent-status-blinking").is_none());
    assert!(cx.debug_bounds("agent-status-still").is_some(), "reduced motion: still");
    cx.update(|_, cx| cx.set_global(crate::status_glyph::Motion { animations: true }));
    h.settle(cx);
    assert!(cx.debug_bounds("agent-status-blinking").is_some(), "it blinks again");
    h.store_mut().set_agent_indicator(AgentIndicator::default(), srv);
    h.settle(cx);
    assert!(cx.debug_bounds("agent-status").is_none());
}

#[gpui_kit::test]
fn the_row_menu_and_the_footers_add_session_menu_run_their_items(cx: &mut TestAppContext) {
    let (library, _) = library();
    let (mut h, cx) = harness(cx, library, AppSettings::default());
    let srv = h.id_of("srv", cx);
    // Rename, Copy Name, Duplicate Session, Move to, Flag: the fifth item
    let at = bounds(cx, "row:srv").center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("down down down down down enter");
    h.settle(cx);
    assert!(h.store().session(srv).unwrap().flagged, "Flag ran on the clicked row");

    let one = h.store().workspaces()[0].id();
    let before = h.store().workspace(one).unwrap().sessions.len();
    let at = bounds(cx, "add-session").center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    h.settle(cx);
    assert_eq!(
        h.store().workspace(one).unwrap().sessions.len(),
        before + 1,
        "New Session in the current workspace"
    );
}
