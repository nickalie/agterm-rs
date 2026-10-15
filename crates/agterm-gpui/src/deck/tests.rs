//! The deck: its plan (ported from the GTK build's `deck.rs` tests), and the deck itself in GPUI's test windows over
//! a real core and real panes: every session mounted and started, hidden pages included; the split laid out at the
//! ratio; keyboard focus following the model; the inactive-pane wash following the focus; a covered pane taken over
//! by a key press.
//!
//! Panes run `/bin/sh` with no `AGTERM_*` or `ZMX_*` variable inherited (CLAUDE.md > Never touch the user's running
//! agterm); the test launcher runs `exec cat` whatever the launch asked, the live session host is a fake.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agterm_core::model::NewSession;
use agterm_core::pane_env::uuid_string;
use agterm_core::persist::RestoreMode;
use agterm_core::restore::mode::RestoreLaunchDecision;
use agterm_core::zmx::support::Inputs;
use agterm_core::{CoreConfig, CoreState, LiveInstall};
use agterm_host::fakes::FakeSessionHost;
use agterm_host::{MainLoop, MainLoopClosed, MainTask, SessionHost};
use agterm_proto::command::SessionCmd;
use agterm_proto::{AppIdentity, Args, Command, ControlZmxEndpoint, Request};
use agterm_term::{Launch, Pane, PaneCommand, PaneConfig};
use gpui_kit::{Entity, TestAppContext, VisualTestContext, px, size};

use super::*;
use crate::pane::tests::three_sessions;
use crate::terminal::Spawn;

const DEADLINE: Duration = Duration::from_secs(20);

// MARK: - the plan

#[test]
fn every_session_gets_a_page_and_the_selected_one_shows() {
    let (library, window, ids) = three_sessions();
    let plan = plan(&library, Some(window), &HashSet::new());
    let added: Vec<Uuid> = plan.add.iter().map(|spec| spec.session).collect();
    assert_eq!(added, ids, "every session, the background one included, in tree order");
    assert!(plan.remove.is_empty());
    assert_eq!(plan.shown, Some(ids[0]));
    let background = &plan.add[2];
    assert_eq!(background.cwd, "/var");
    assert_eq!(background.command.as_deref(), Some("htop"));
    assert!(background.wait);
}

#[test]
fn mounted_pages_stay_and_gone_sessions_lose_theirs() {
    let (mut library, window, ids) = three_sessions();
    let mounted: HashSet<Uuid> = ids.iter().copied().collect();
    assert_eq!(
        plan(&library, Some(window), &mounted),
        DeckPlan {
            shown: Some(ids[0]),
            ..DeckPlan::default()
        }
    );
    let store = library.store_mut(window).unwrap();
    store.close_session(ids[0]);
    let plan = plan(&library, Some(window), &mounted);
    assert!(plan.add.is_empty());
    assert_eq!(plan.remove, vec![ids[0]]);
    assert_eq!(plan.shown, library.store(window).unwrap().selected_session_id());
    assert_ne!(plan.shown, Some(ids[0]));
}

#[test]
fn a_session_a_soft_close_holds_keeps_its_hidden_page() {
    let (mut library, window, ids) = three_sessions();
    let mounted: HashSet<Uuid> = ids.iter().copied().collect();
    let store = library.store_mut(window).unwrap();
    assert!(store.soft_close_session(ids[1]));
    store.close_session(ids[2]);
    let plan = plan(&library, Some(window), &mounted);
    assert_eq!(
        plan.remove,
        vec![ids[2]],
        "the held session's page stays, the hard-closed one's goes"
    );
    assert!(plan.add.is_empty(), "a held session out of the tree gets no new page");
    assert_ne!(plan.shown, Some(ids[1]), "and never shows");
}

#[test]
fn no_selection_shows_no_page_and_a_closed_window_drops_every_page() {
    let (mut library, window, ids) = three_sessions();
    let mounted: HashSet<Uuid> = ids.iter().copied().collect();
    library.store_mut(window).unwrap().select_session(None);
    assert_eq!(plan(&library, Some(window), &mounted).shown, None);
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(plan(&library, None, &mounted).remove, sorted);
    library.new_window(None);
    library.close_window(window);
    let closed = plan(&library, Some(window), &mounted);
    assert_eq!(closed.remove, sorted);
    assert!(closed.add.is_empty() && closed.shown.is_none());
}

#[test]
fn other_windows_sessions_get_no_page() {
    let (mut library, window, ids) = three_sessions();
    let other = library.new_window(None);
    let other_session = library.store(other).unwrap().sessions().next().unwrap().id();
    let plan = plan(&library, Some(window), &HashSet::new());
    assert_eq!(plan.add.len(), ids.len());
    assert!(
        plan.add
            .iter()
            .all(|spec| spec.session != other_session && spec.window == window)
    );
}

#[test]
fn a_replacement_runs_over_the_panes_own_environment() {
    let base = vec![("A".to_owned(), "1".to_owned()), ("B".to_owned(), "2".to_owned())];
    let overlay = vec![("B".to_owned(), "3".to_owned()), ("C".to_owned(), "4".to_owned())];
    assert_eq!(
        merged_env(&base, &overlay),
        [
            ("A".to_owned(), "1".to_owned()),
            ("B".to_owned(), "3".to_owned()),
            ("C".to_owned(), "4".to_owned())
        ]
    );
}

// MARK: - the deck in a window

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

/// `/bin/sh -c 'exec cat'` with the inherited `AGTERM_*`/`ZMX_*` variables removed.
fn quiet_shell() -> PaneCommand {
    let mut command = PaneCommand::new("/bin/sh").arg("-c").arg("exec cat");
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
            command = command.env_remove(key);
        }
    }
    command
}

/// A launcher that starts [`quiet_shell`] whatever it is asked to launch, keeping each launch.
fn test_launcher(launches: Arc<Mutex<Vec<Launch>>>) -> pane::Launcher {
    Rc::new(move |launch: &Launch| -> Spawn {
        launches.lock().unwrap().push(launch.clone());
        let command = quiet_shell();
        Box::new(move |config: PaneConfig| Pane::spawn(&command, &config))
    })
}

/// A core over `library`, its state attached to a recording loop, a deck of `window` in a test window.
struct Harness<'a> {
    state: AppState,
    posted: Arc<Posted>,
    _loop: Arc<dyn MainLoop<AppState>>,
    deck: Entity<Deck>,
    window: Uuid,
    launches: Arc<Mutex<Vec<Launch>>>,
    state_dir: tempfile::TempDir,
    cx: &'a mut VisualTestContext,
}

fn core_of(library: WindowLibrary) -> Core {
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

fn harness(cx: &mut TestAppContext, core: Core, window: Uuid) -> Harness<'_> {
    // the panes' threads wake their pumps from outside the test scheduler
    cx.executor().allow_parking();
    let posted = Arc::new(Posted::default());
    let main_loop: Arc<dyn MainLoop<AppState>> = Arc::clone(&posted) as _;
    let mut state = AppState::new(core);
    state.attach(Arc::downgrade(&main_loop));
    let launches = Arc::new(Mutex::new(Vec::new()));
    let state_dir = tempfile::tempdir().unwrap();
    let env = Rc::new(DeckEnv::new(
        PaneContext {
            socket: "/tmp/agterm-test.sock".to_owned(),
            program_version: "1.2.3".to_owned(),
        },
        test_launcher(Arc::clone(&launches)),
        TerminalConfig::default(),
        Arc::downgrade(&main_loop),
        Watermarks {
            state_dir: state_dir.path().to_path_buf(),
            config_with: Rc::new(config_with),
        },
    ));
    let (deck, cx) = cx.add_window_view(|_, _| Deck::new(window, env));
    cx.simulate_scale_factor_change(1.0);
    cx.simulate_resize(size(px(803.), px(600.)));
    // an active window, so focus moves reach the focus listeners as on a desktop
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    Harness {
        state,
        posted,
        _loop: main_loop,
        deck,
        window,
        launches,
        state_dir,
        cx,
    }
}

/// The default config with `overlay` loaded over it, as the app loads a terminal's own overlay over its config.
pub(crate) fn config_with(overlay: &str) -> TerminalConfig {
    let mut loader = agterm_core::ghostty_config::Loader::new(None);
    loader.load_text("overlay", overlay);
    TerminalConfig::from_ghostty(&loader.finish().finalize(false, &[]).config)
}

impl Harness<'_> {
    /// Runs the posted tasks, reconciles the deck, hands the granted panes their seeds and lets the app run: one turn
    /// of the app's loop and the windows' sync.
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
        let (deck, state) = (self.deck.clone(), &mut self.state);
        let focused = self
            .cx
            .update(|window, cx| deck.update(cx, |deck, cx| deck.reconcile(&mut state.core, false, window, cx)));
        if focused {
            self.state.settle();
        }
        self.state.core.clear_pane_focus_requests();
        for grant in self.state.take_spawns() {
            self.deck.read_with(self.cx, |deck, _| deck.launch(&grant));
        }
        self.cx.run_until_parked();
    }

    /// Turns until `done` holds (the panes' threads run meanwhile), or fails.
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

    fn store(&mut self) -> &mut agterm_core::model::AppStore {
        self.state.core.library_mut().store_mut(self.window).unwrap()
    }

    fn session(&self, id: Uuid) -> &Session {
        self.state
            .core
            .library()
            .store(self.window)
            .unwrap()
            .session(id)
            .unwrap()
    }

    /// The token of `session`'s pane in role `pane`.
    fn token(&self, session: Uuid, pane: StatusPane) -> String {
        self.session(session)
            .pane_slot(pane)
            .get()
            .map(|surface| surface.pane_token().to_owned())
            .expect("the pane has a terminal")
    }

    fn realized(&self, session: Uuid, pane: StatusPane) -> bool {
        self.session(session)
            .pane_slot(pane)
            .get()
            .is_some_and(agterm_host::Surface::is_realized)
    }

    fn shared(&mut self, session: Uuid, token: &str) -> Rc<Shared> {
        self.deck.read_with(self.cx, |deck, _| {
            Rc::clone(deck.terminal(session, token).expect("a terminal").shared())
        })
    }

    /// Whether the terminal carrying `token` holds the window's keyboard.
    fn focused(&mut self, session: Uuid, token: &str) -> bool {
        let deck = self.deck.clone();
        let token = token.to_owned();
        self.cx.update(|window, cx| {
            let handle = deck
                .read(cx)
                .terminal(session, &token)
                .map(|pane| pane.focus_handle(cx));
            handle.is_some_and(|handle| handle.is_focused(window))
        })
    }

    fn shown(&mut self, session: Uuid, token: &str) -> bool {
        self.deck.read_with(self.cx, |deck, cx| {
            deck.terminal(session, token)
                .is_some_and(|pane| pane.view.read(cx).is_shown())
        })
    }

    /// The pane's grid, as its program sees it.
    fn cols(&mut self, session: Uuid, token: &str) -> u16 {
        self.shared(session, token)
            .pane()
            .map_or(0, |pane| pane.frame().frame.cols)
    }

    fn dispatch(&mut self, cmd: Command, session: Uuid, edit: impl FnOnce(&mut Args)) {
        let mut args = Args::default();
        edit(&mut args);
        let response = self.state.core.dispatch(&Request {
            cmd,
            target: Some(uuid_string(session)),
            args: Some(args),
        });
        assert!(response.ok, "{response:?}");
        self.state.settle();
    }
}

#[gpui_kit::test]
fn every_session_mounts_and_starts_while_only_the_selected_one_shows(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    h.until("every pane to start", |h| {
        ids.iter().all(|id| h.realized(*id, StatusPane::Left))
    });
    let launched: Vec<Option<String>> = h
        .launches
        .lock()
        .unwrap()
        .iter()
        .map(|launch| launch.command.clone())
        .collect();
    assert_eq!(
        launched.len(),
        3,
        "each pane launched once, with its seed: {launched:?}"
    );
    assert!(
        launched.contains(&Some("htop".to_owned())),
        "the background session's command"
    );
    for (index, id) in ids.iter().enumerate() {
        let token = h.token(*id, StatusPane::Left);
        assert_eq!(h.shown(*id, &token), index == 0, "only the selected page draws");
    }
    h.deck.read_with(h.cx, |deck, _| assert_eq!(deck.shown(), Some(ids[0])));
    let first = h.token(ids[0], StatusPane::Left);
    assert!(h.focused(ids[0], &first), "the shown pane has the keyboard");

    // selecting another session is a visibility flip, never a new terminal
    h.store().select_session(Some(ids[2]));
    h.state.settle();
    h.turn();
    let third = h.token(ids[2], StatusPane::Left);
    assert!(h.shown(ids[2], &third) && !h.shown(ids[0], &first));
    assert!(h.focused(ids[2], &third), "the keyboard follows the selection");
    assert_eq!(h.launches.lock().unwrap().len(), 3);

    // a closed session's page goes
    h.store().close_session(ids[1]);
    h.state.settle();
    h.turn();
    h.deck.read_with(h.cx, |deck, _| assert!(deck.page(ids[1]).is_none()));
}

#[gpui_kit::test]
fn a_shown_split_lays_both_panes_out_at_the_ratio_and_focus_follows_the_model(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let session = ids[0];
    h.until("the primary to start", |h| h.realized(session, StatusPane::Left));
    let primary = h.token(session, StatusPane::Left);
    // the fake face's 10 px cell, 2 px of padding each side: 803 px across is 79 columns
    h.until("the lone pane's grid", |h| h.cols(session, &primary) == 79);

    h.dispatch(Command::Session(SessionCmd::Split), session, |_| {});
    h.until("the split pane to start", |h| h.realized(session, StatusPane::Right));
    let split = h.token(session, StatusPane::Right);
    assert_eq!(
        h.session(session).split.ratio,
        Some(agterm_core::model::pane::SPLIT_RATIO_DEFAULT),
        "a shown split reports its ratio"
    );
    // 802 px of content split evenly: 401 each, 39 columns
    h.until("the even split", |h| {
        h.cols(session, &primary) == 39 && h.cols(session, &split) == 39
    });
    assert!(h.shown(session, &primary) && h.shown(session, &split));
    assert!(h.focused(session, &split), "a new split's pane takes the keyboard");

    // a dragged ratio stored by the model lays the panes out anew
    h.state.set_split_ratio(session, 0.25);
    h.turn();
    // 802 * 0.25 = 200.5 rounds to 201 px for the primary, 601 px for the split: 19 and 59 columns
    h.until("the moved divider", |h| {
        h.cols(session, &primary) == 19 && h.cols(session, &split) == 59
    });

    // `session.focus` moves the keyboard to the pane it names
    h.dispatch(Command::Session(SessionCmd::Focus), session, |args| {
        args.pane = Some("left".to_owned());
    });
    h.turn();
    assert!(h.focused(session, &primary) && !h.focused(session, &split));
    assert!(!h.session(session).split.focused, "the model's split focus moved");

    // hiding the split shows the focused pane alone, the other one keeps running unseen
    h.dispatch(Command::Session(SessionCmd::Split), session, |args| {
        args.mode = Some("off".to_owned());
    });
    h.turn();
    assert!(h.shown(session, &primary) && !h.shown(session, &split));
    assert!(h.realized(session, StatusPane::Right));
    h.until("the lone pane's grid again", |h| h.cols(session, &primary) == 79);
}

#[gpui_kit::test]
fn queued_focus_moves_settle_on_the_last_one(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let session = ids[0];
    h.dispatch(Command::Session(SessionCmd::Split), session, |_| {});
    h.until("both panes to start", |h| {
        h.realized(session, StatusPane::Left) && h.realized(session, StatusPane::Right)
    });
    let (primary, split) = (h.token(session, StatusPane::Left), h.token(session, StatusPane::Right));

    // the deck's own focus moves reach the model as they happen: none waits in the queue to undo a later one
    for _ in 0..3 {
        for (pane, token) in [("left", &primary), ("right", &split)] {
            h.dispatch(Command::Session(SessionCmd::Focus), session, |args| {
                args.pane = Some(pane.to_owned());
            });
            h.turn();
            assert!(h.focused(session, token), "{pane} holds the keyboard");
        }
    }
    h.turn();
    h.turn();
    assert!(h.focused(session, &split), "the last move holds the keyboard");
    assert!(h.session(session).split.focused, "the model's focus is the last move's");
}

/// Gives the terminal carrying `token` the keyboard as a click would, and lets its focus listeners run.
fn click_focus(h: &mut Harness<'_>, session: Uuid, token: &str) {
    let deck = h.deck.clone();
    h.cx.update(|window, cx| {
        let handle = deck.read(cx).terminal(session, token).unwrap().focus_handle(cx);
        window.focus(&handle, cx);
    });
    h.cx.run_until_parked();
}

/// Reconciles the deck without running the queued tasks first: a sync that comes before them.
fn reconcile_only(h: &mut Harness<'_>) -> bool {
    let (deck, state) = (h.deck.clone(), &mut h.state);
    h.cx.update(|window, cx| deck.update(cx, |deck, cx| deck.reconcile(&mut state.core, false, window, cx)))
}

#[gpui_kit::test]
fn a_users_focus_move_outlasts_a_sync_before_its_report(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let session = ids[0];
    h.dispatch(Command::Session(SessionCmd::Split), session, |_| {});
    h.until("both panes to start", |h| {
        h.realized(session, StatusPane::Left) && h.realized(session, StatusPane::Right)
    });
    let (primary, split) = (h.token(session, StatusPane::Left), h.token(session, StatusPane::Right));
    assert!(h.session(session).split.focused, "the new split's pane has the focus");

    // a click's report is queued behind whatever the main loop runs first; a sync before it keeps the user's pane
    click_focus(&mut h, session, &primary);
    assert!(!reconcile_only(&mut h), "the deck gave no focus of its own");
    assert!(h.focused(session, &primary), "the sync did not take the click back");
    h.turn();
    assert!(!h.session(session).split.focused, "the click's report landed");
    assert!(h.focused(session, &primary));

    // a report a later move overtook is dropped when it runs
    click_focus(&mut h, session, &split);
    h.store().select_session(Some(ids[1]));
    h.state.settle();
    let _gave = reconcile_only(&mut h);
    assert_eq!(h.deck.read_with(h.cx, |deck, _| deck.shown()), Some(ids[1]));
    h.turn();
    assert!(
        !h.session(session).split.focused,
        "the overtaken report changed nothing"
    );
}

#[gpui_kit::test]
fn the_inactive_pane_wash_follows_the_focus(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let session = ids[0];
    h.dispatch(Command::Session(SessionCmd::Split), session, |_| {});
    h.until("both panes to start", |h| {
        h.realized(session, StatusPane::Left) && h.realized(session, StatusPane::Right)
    });
    let wash = |h: &mut Harness<'_>| {
        h.deck
            .read_with(h.cx, |deck, _| deck.page(session).unwrap().washes().pane)
    };
    let muted = wash(&mut h).expect("the unfocused pane is washed");
    assert_eq!(muted.slot, Slot::Primary, "the split has the focus");
    assert!(!muted.overlay);
    assert!((muted.opacity - 0.4).abs() < 1e-9, "the default strength 5");

    h.dispatch(Command::Session(SessionCmd::Focus), session, |args| {
        args.pane = Some("left".to_owned());
    });
    h.turn();
    assert_eq!(
        wash(&mut h).map(|muted| muted.slot),
        Some(Slot::Split),
        "the wash moved"
    );

    // a page off screen washes nothing
    h.store().select_session(Some(ids[1]));
    h.state.settle();
    h.turn();
    assert_eq!(wash(&mut h), None);
}

/// A resources directory holding the zsh integration a live wrap checks for.
fn live_resources() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let integration = dir.path().join("shell-integration/zsh");
    std::fs::create_dir_all(&integration).unwrap();
    std::fs::write(integration.join(".zshenv"), "").unwrap();
    dir
}

/// The core of `library` with live sessions over a fake session host.
fn live_core(library: WindowLibrary, resources: &Path) -> Core {
    let mut core = core_of(library);
    core.set_restore_launch(RestoreLaunchDecision::new(RestoreMode::Live, None));
    core.install_live_sessions(LiveInstall {
        host: Arc::new(FakeSessionHost::new()) as Arc<dyn SessionHost>,
        endpoint: ControlZmxEndpoint {
            executable: "/opt/agterm/bin/zmx".to_owned(),
            socket_directory: "/tmp/agterm-zmx-test".to_owned(),
        },
        wrap: Inputs {
            zmx_executable: "/bin/sh".into(),
            login_shell: Some("/bin/zsh".to_owned()),
            resources_dir: Some(resources.to_path_buf()),
            state_dir: resources.join("state"),
            ..Inputs::default()
        },
        state_dir: resources.to_path_buf(),
        outdated_before: None,
    });
    core
}

#[gpui_kit::test]
fn a_key_press_on_a_covered_pane_takes_it_over_in_a_fresh_terminal(cx: &mut TestAppContext) {
    let resources = live_resources();
    let mut library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let store = library.store_mut(window).unwrap();
    let session = store.sessions().next().unwrap().id();
    let workspace = store.workspaces()[0].id();
    store.add_session(workspace, NewSession::at_cwd("/srv"));
    store.select_session(Some(session));
    let mut h = harness(cx, live_core(library, resources.path()), window);
    h.until("the live pane to start", |h| h.realized(session, StatusPane::Left));
    let token = h.token(session, StatusPane::Left);
    let nonce = {
        let launches = h.launches.lock().unwrap();
        let first = launches
            .iter()
            .find(|launch| launch.env.iter().any(|(key, _)| key == "ZMX_MANAGED"));
        first
            .and_then(|launch| {
                launch
                    .env
                    .iter()
                    .find(|(key, _)| key == "ZMX_MANAGED")
                    .map(|(_, value)| value.clone())
            })
            .expect("a live launch carries its attachment")
    };
    let shared = h.shared(session, &token);
    assert!(shared.is_live(), "the seed made the terminal a live client");
    assert!(!shared.holds_keys());

    // another client leads: the pane is covered and holds its keys
    let report = agterm_core::PaneReport::Title(format!("zmx-role;{nonce}:follower:1"));
    h.state.report_pane(session, &token, &report);
    h.turn();
    let cover = h
        .deck
        .read_with(h.cx, |deck, _| deck.page(session).unwrap().cover(&token).cloned());
    let cover = cover.expect("the pane has a cover");
    assert_eq!(cover.cover.map(|lead| lead.title), Some("In use from another machine"));
    assert!(cover.holds_keys && shared.holds_keys());
    assert!(h.focused(session, &token));

    // a key press takes the pane over: the core asks for a fresh attach with the claim, which replaces the terminal
    h.cx.simulate_keystrokes("a");
    h.turn();
    let replacements = h.state.core.take_pane_replacements();
    assert_eq!(replacements.len(), 1, "one takeover");
    let replacement = &replacements[0];
    assert_eq!(
        (replacement.session, replacement.token.as_str()),
        (session, token.as_str())
    );
    assert!(
        replacement
            .env
            .iter()
            .any(|(key, value)| key == "ZMX_MANAGED_CLAIM" && value == "1")
    );
    let (deck, state) = (h.deck.clone(), &mut h.state);
    let replaced = h.cx.update(|window, cx| {
        deck.update(cx, |deck, cx| {
            deck.replace_pane(&mut state.core, replacement, window, cx)
        })
    });
    assert!(replaced);
    h.state.core.pane_replaced(replacement);
    h.turn();
    assert!(shared.is_torn_down(), "the covered terminal is gone");
    let fresh = h.shared(session, &token);
    assert!(!Rc::ptr_eq(&fresh, &shared), "a fresh terminal carries the pane");
    assert_eq!(h.token(session, StatusPane::Left), token, "with the pane's own token");
    h.until("the fresh terminal to start", |h| h.realized(session, StatusPane::Left));
    assert!(fresh.is_live());
    let cover = h
        .deck
        .read_with(h.cx, |deck, _| deck.page(session).unwrap().cover(&token).cloned());
    assert_eq!(
        cover.and_then(|cover| cover.cover).map(|lead| lead.title),
        Some("Taking over…"),
        "covered until the fresh attach reports"
    );
}

#[gpui_kit::test]
fn a_held_exit_closes_its_pane_on_the_next_key(cx: &mut TestAppContext) {
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let session = ids[0];
    h.until("the pane to start", |h| h.realized(session, StatusPane::Left));
    let token = h.token(session, StatusPane::Left);
    let shared = h.shared(session, &token);
    let status = agterm_term::ExitStatus {
        code: 1,
        signal: None,
        held: true,
    };
    h.deck.update_in(h.cx, |deck, window, cx| {
        let event = TerminalEvent::Pane(agterm_term::PaneEvent::Exited(status));
        deck.terminal_event(session, &token, &event, window, cx);
    });
    assert!(shared.holds_keys(), "a held exit waits for a key");
    h.turn();
    assert!(h.state.core.library().store(window).unwrap().session(session).is_some());
    h.cx.simulate_keystrokes("x");
    h.turn();
    assert!(
        h.state.core.library().store(window).unwrap().session(session).is_none(),
        "the key closed the pane, and its session with it"
    );
}

#[gpui_kit::test]
fn a_session_background_reaches_its_terminals_and_a_pane_override_wins(cx: &mut TestAppContext) {
    use agterm_core::model::{BackgroundWatermark, WatermarkKind};
    let (library, window, ids) = three_sessions();
    let mut h = harness(cx, core_of(library), window);
    let session = ids[0];
    h.dispatch(Command::Session(SessionCmd::Split), session, |_| {});
    h.until("both panes to start", |h| {
        h.realized(session, StatusPane::Left) && h.realized(session, StatusPane::Right)
    });
    let (left, right) = (h.token(session, StatusPane::Left), h.token(session, StatusPane::Right));
    let background = |h: &mut Harness<'_>, token: &str| {
        let theme = h.shared(session, token).config().theme.background;
        (theme.r, theme.g, theme.b)
    };
    let default = background(&mut h, &left);
    let color = |hex: &str| BackgroundWatermark {
        kind: WatermarkKind::Color,
        image_path: None,
        text: None,
        color_hex: Some(hex.to_owned()),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    };
    h.store().session_mut(session).unwrap().background_watermark = Some(color("#201414"));
    h.turn();
    assert_eq!(background(&mut h, &left), (0x20, 0x14, 0x14), "the session default");
    assert_eq!(background(&mut h, &right), (0x20, 0x14, 0x14));
    h.store().session_mut(session).unwrap().pane_backgrounds.right = Some(color("#0a0b0c"));
    h.turn();
    assert_eq!(background(&mut h, &left), (0x20, 0x14, 0x14));
    assert_eq!(background(&mut h, &right), (0x0a, 0x0b, 0x0c), "the pane's own wins");

    // a text background is drawn to its file under the state dir
    h.store().session_mut(session).unwrap().background_watermark = Some(BackgroundWatermark {
        kind: WatermarkKind::Text,
        text: Some("PROD".to_owned()),
        color_hex: None,
        ..color("#000000")
    });
    h.turn();
    let file = agterm_core::watermark::rendered_text_path(h.state_dir.path(), session, None);
    assert!(
        file.is_file(),
        "the left pane's inherited text is drawn to {}",
        file.display()
    );
    assert_eq!(
        background(&mut h, &left),
        default,
        "an image leaves the background color alone"
    );

    // cleared: the overlay goes, and so does the text file no terminal draws any more
    h.store().session_mut(session).unwrap().background_watermark = None;
    h.store().session_mut(session).unwrap().pane_backgrounds.right = None;
    h.turn();
    assert_eq!(background(&mut h, &left), default);
    assert_eq!(background(&mut h, &right), default);
    assert!(!file.exists(), "the stale text image is removed");
}
