//! The popovers in GPUI's test window over a real core: opening and closing (the core hears both), the keyboard's
//! row and Return running it, Escape and a press outside closing, a button that turned off closing its popover. The
//! popovers' posts land on a recording loop the test runs against the app state, then they sync as the window would.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agterm_core::model::{NewSession, WindowLibrary};
use agterm_core::{Core, CoreConfig, CoreState};
use agterm_host::{MainLoop, MainLoopClosed, MainTask};
use agterm_proto::AppIdentity;
use gpui_kit::{Entity, TestAppContext, VisualTestContext, point, px, size};

use super::*;

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

struct Harness {
    state: AppState,
    posted: Arc<Posted>,
    view: Entity<Popovers>,
    window: Uuid,
    sessions: Vec<Uuid>,
}

/// One workspace with sessions "u", "srv" and "var", each selected in turn, so the first two are recent.
fn harness(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let workspace = store.workspaces()[0].id();
    let first = store.sessions().next().unwrap().id();
    let srv = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
    let var = store.add_session(workspace, NewSession::at_cwd("/var")).unwrap();
    for id in [first, srv, var] {
        store.select_session(Some(id));
    }
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
    let weak: Poster = Arc::downgrade(&posted) as Poster;
    let mut state = AppState::new(core);
    state.attach(weak.clone());
    let (view, cx) = cx.add_window_view(move |_, cx| Popovers::new(window, weak, cx));
    cx.simulate_resize(size(px(800.), px(500.)));
    cx.update(|window, _| window.activate_window());
    let mut harness = Harness {
        state,
        posted,
        view,
        window,
        sessions: vec![first, srv, var],
    };
    harness.settle(cx);
    (harness, cx)
}

impl Harness {
    /// Runs what was posted and syncs the popovers, until nothing more is posted.
    fn settle(&mut self, cx: &mut VisualTestContext) {
        for _ in 0..8 {
            cx.run_until_parked();
            let tasks: Vec<MainTask<AppState>> = std::mem::take(&mut *self.posted.0.lock().unwrap());
            for task in tasks {
                task(&mut self.state);
            }
            let core = &self.state.core;
            self.view.update(cx, |popovers, cx| popovers.sync(core, cx));
            cx.run_until_parked();
            if self.posted.0.lock().unwrap().is_empty() {
                return;
            }
        }
    }

    fn open(&mut self, kind: PopoverKind, cx: &mut VisualTestContext) {
        let view = self.view.clone();
        cx.update(|window, cx| {
            view.update(cx, |popovers, cx| {
                popovers.toggle(kind, point(px(400.), px(48.)), window, cx);
            });
        });
        self.settle(cx);
    }

    fn selected(&self) -> Option<Uuid> {
        self.state
            .core
            .library()
            .store(self.window)
            .and_then(agterm_core::model::AppStore::selected_session_id)
    }
}

fn holds_keyboard(view: &Entity<Popovers>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| view.read(cx).focus_handle().is_focused(window))
}

#[gpui_kit::test]
fn the_recent_popover_lists_and_runs_its_rows_by_keyboard(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx);
    h.open(PopoverKind::Recent, cx);
    assert_eq!(h.view.read_with(cx, |p, _| p.open_kind()), Some(PopoverKind::Recent));
    let rows = h.view.read_with(cx, |p, _| p.row_titles());
    assert!(!rows.is_empty(), "the other sessions are recent: {rows:?}");
    assert!(
        !rows.contains(&"var".to_owned()),
        "the selected one is no recent row: {rows:?}"
    );
    assert!(holds_keyboard(&h.view, cx), "the popover takes the keyboard");

    cx.simulate_keystrokes("down enter");
    h.settle(cx);
    assert_eq!(h.view.read_with(cx, |p, _| p.open_kind()), None, "a run row closes it");
    let expected = h.sessions.iter().copied().find(|id| {
        h.state
            .core
            .library()
            .store(h.window)
            .unwrap()
            .session(*id)
            .unwrap()
            .display_name()
            == rows[0]
    });
    assert_eq!(h.selected(), expected, "the first row's session is selected");
}

#[gpui_kit::test]
fn escape_a_press_outside_and_a_second_click_close_it(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx);
    h.open(PopoverKind::Recent, cx);
    cx.simulate_keystrokes("escape");
    h.settle(cx);
    assert!(!h.view.read_with(cx, |p, _| p.is_open()), "Escape closes");

    h.open(PopoverKind::Recent, cx);
    cx.simulate_click(point(px(20.), px(450.)), gpui_kit::Modifiers::default());
    h.settle(cx);
    assert!(!h.view.read_with(cx, |p, _| p.is_open()), "a press outside closes");
    let before = h.selected();
    assert_eq!(before, Some(h.sessions[2]), "and runs nothing");

    h.open(PopoverKind::Recent, cx);
    h.open(PopoverKind::Recent, cx);
    assert!(!h.view.read_with(cx, |p, _| p.is_open()), "its button again closes it");
}

#[gpui_kit::test]
fn a_button_turned_off_closes_its_popover_and_opens_none(cx: &mut TestAppContext) {
    let (mut h, cx) = harness(cx);
    // the attention button is off by default: its popover never opens
    h.open(PopoverKind::Attention, cx);
    assert!(!h.view.read_with(cx, |p, _| p.is_open()));

    h.open(PopoverKind::Recent, cx);
    assert!(h.view.read_with(cx, |p, _| p.is_open()));
    h.state.core.update_settings(|settings| {
        settings.hidden_interface_elements = Some(vec!["recentSessions".to_owned()]);
    });
    h.settle(cx);
    assert!(!h.view.read_with(cx, |p, _| p.is_open()), "its button went away");
}
