//! The on-screen windows, one per open model window (upstream's `WindowRegistry.swift`; ported from the GTK build's
//! `windows.rs`): opened when a store opens, closed when it closes, each over its own store with its own deck.
//!
//! The model's open set drives it. After every main-loop task [`Windows::sync`] closes the windows of closed stores,
//! opens one for each newly opened store (the frontmost last, so it ends up on top), renders every window, then applies
//! what the window commands asked of them ([`WindowRequest`]) and hands the granted panes their seeds. A terminal never
//! moves between windows: a session's page lives in its own window's deck (upstream has no cross-window session move
//! either). Once no window is open the app quits, as upstream's `applicationShouldTerminateAfterLastWindowClosed` keys
//! termination off the model's open set.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::rc::Rc;

use agterm_core::{Core, WindowRequest};
use agterm_proto::ControlKeymapMenuItem;
use agterm_term::Host;
use gpui_kit::{App, Entity, WindowHandle};
use uuid::Uuid;

use crate::deck::{Deck, DeckEnv};
use crate::instance::Activation;
use crate::quick::{QuickEnv, QuickPanel};
use crate::session_host::AppConfig;
use crate::settings::{Opening, Snapshot};
use crate::state::AppState;
use crate::terminal::TerminalConfig;
use crate::window::WindowView;
use crate::window_frames::WindowFrames;

/// One reconcile of the on-screen windows against the open set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowsPlan {
    /// Windows whose store closed (or was deleted).
    pub close: Vec<Uuid>,
    /// Open windows without an on-screen window yet, in the order to present them: window order, the frontmost last.
    pub build: Vec<Uuid>,
}

/// What [`Windows::sync`] closes and builds, given the `open` window ids in window order, the windows `shown`, and the
/// `frontmost` one.
#[must_use]
pub fn plan<S: BuildHasher>(open: &[Uuid], shown: &HashSet<Uuid, S>, frontmost: Option<Uuid>) -> WindowsPlan {
    let mut close: Vec<Uuid> = shown.iter().copied().filter(|id| !open.contains(id)).collect();
    close.sort_unstable();
    let mut build: Vec<Uuid> = open.iter().copied().filter(|id| !shown.contains(id)).collect();
    if let Some(index) = build.iter().position(|id| Some(*id) == frontmost) {
        let front = build.remove(index);
        build.push(front);
    }
    WindowsPlan { close, build }
}

/// One window on screen.
pub struct WindowUi {
    pub handle: WindowHandle<WindowView>,
    pub deck: Entity<Deck>,
}

/// Where the config, the launch host and the panes' shared environment live, for a reload to replace.
pub struct ConfigHost {
    pub resources: Option<std::path::PathBuf>,
    pub host: Rc<RefCell<Host>>,
    /// The side of the desktop's appearance the config loads on, which the terminals' overlays load on too.
    pub dark: Rc<std::cell::Cell<bool>>,
}

/// The windows by model window id.
pub struct Windows {
    env: Rc<DeckEnv>,
    app_id: String,
    config: Option<ConfigHost>,
    shown: HashMap<Uuid, WindowUi>,
    /// What this launch was handed, for the first window to open.
    launched: Option<Activation>,
    /// The app's quick terminal panel, which belongs to no window.
    quick: QuickPanel,
    /// The frontmost window's app menu chords as last synced, which `keymap.list` reads ([`Windows::menu_reader`]).
    menu_items: Rc<RefCell<Vec<ControlKeymapMenuItem>>>,
    /// The windows' saved frames, each given back once when its window opens ([`crate::window_frames`]).
    frames: WindowFrames,
}

impl Windows {
    #[must_use]
    pub fn new(env: Rc<DeckEnv>, app_id: String, config: Option<ConfigHost>, launched: Option<Activation>) -> Self {
        let frames = WindowFrames::load(&env.watermarks.state_dir);
        Self {
            env,
            app_id,
            config,
            shown: HashMap::new(),
            launched,
            quick: QuickPanel::default(),
            menu_items: Rc::default(),
            frames,
        }
    }

    /// What `keymap.list` reads the app menu's live chords with: the frontmost window's menu as the last sync left it
    /// (the GTK build reads its menu model the same way).
    #[must_use]
    pub fn menu_reader(&self) -> agterm_core::MenuReader {
        let items = Rc::clone(&self.menu_items);
        Box::new(move || items.borrow().clone())
    }

    /// The panes' shared environment.
    #[must_use]
    pub fn env(&self) -> &Rc<DeckEnv> {
        &self.env
    }

    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<&WindowUi> {
        self.shown.get(&id)
    }

    /// The windows on screen.
    pub fn iter(&self) -> impl Iterator<Item = (&Uuid, &WindowUi)> {
        self.shown.iter()
    }

    /// The frontmost agterm window (the model's active window), when shown.
    #[must_use]
    pub fn frontmost(&self, core: &Core) -> Option<&WindowUi> {
        core.library().active_window_id().and_then(|id| self.shown.get(&id))
    }

    /// Brings the windows in line with the model (module docs), then reports every window's state to the core, lands
    /// the pane focus requests and the spawn grants. Whether the model changed so that it needs another settle.
    pub fn sync(&mut self, state: &mut AppState, cx: &mut App) -> bool {
        if let Some(reload) = state.take_config_reload() {
            // a flip alone keeps every terminal's font size, as upstream's `reloadConfigPreservingSessionZoom` does
            let count = self.reload_config(reload.dark, !reload.asked, cx);
            state.config_applied(reload.dark);
            if reload.asked {
                state.core.report_config_reload(count);
            }
        }
        // after any reload, so the chrome takes the colors the terminals now render; before the windows render, so a
        // new window's first frame has them
        crate::chrome::sync(&state.core, &self.env.config.borrow(), cx);
        // before the decks: a released page's element leaves its slot first
        crate::html_overlay::settle(&mut state.core, cx);
        // a dashboard follows the model before anything renders it
        state.core.reconcile_dashboards();
        let mut resettle = self.replace_panes(&mut state.core, cx);
        let built = self.open_and_close(&mut state.core, cx);
        for ui in self.shown.values() {
            let synced = ui
                .handle
                .update(cx, |view, window, cx| view.sync(&mut state.core, window, cx));
            if let Ok(focused) = synced {
                resettle |= focused;
            } else {
                tracing::debug!("a window is gone before its sync");
            }
        }
        self.present_built(&built, cx);
        for request in state.core.take_window_requests() {
            self.apply(request, cx);
        }
        let quick = QuickEnv {
            env: &self.env,
            app_id: &self.app_id,
            frontmost: self.frontmost(&state.core).map(|ui| ui.handle.into()),
        };
        let held = crate::quick::holds_keyboard(cx);
        self.quick.sync(&mut state.core, &quick, cx);
        // the panel went with the keyboard: the decks, which stepped aside for it, focus their terminals again
        resettle |= held && !crate::quick::holds_keyboard(cx);
        self.sync_settings(state, cx);
        *self.menu_items.borrow_mut() = state
            .core
            .library()
            .active_window_id()
            .map(|id| crate::app_menu::AppMenu::of(&state.core, id).keymap_items())
            .unwrap_or_default();
        self.report_states(&mut state.core, cx);
        state.core.clear_pane_focus_requests();
        self.launch(state, cx);
        if self.shown.is_empty() {
            tracing::info!("no window is open: quitting");
            state.request_quit();
        }
        resettle
    }

    /// Opens the Settings popup over what the core held (`opening`), over model window `host`, the `frontmost` one
    /// when `host` is not on screen (it closed after its menu asked); while one is up anywhere, brings its window to
    /// the front with the popup holding the keyboard instead (unless a modal over it holds it): there is one at a time
    /// (`crate::settings`).
    pub fn present_settings(&self, opening: Opening, host: Uuid, frontmost: Option<Uuid>, cx: &mut App) {
        if let Some((_, ui)) = self.settings_ui(cx) {
            let raised = ui.handle.update(cx, |view, window, cx| {
                window.activate_window();
                // the front holder takes the keyboard: the popup, or a picker, GUI ask, palette or alert over it
                view.focus_terminal(window, cx);
            });
            if raised.is_err() {
                tracing::debug!("the Settings popup's window is gone");
            }
            return;
        }
        let Some(ui) = self
            .shown
            .get(&host)
            .or_else(|| frontmost.and_then(|id| self.shown.get(&id)))
        else {
            tracing::debug!("Settings: no window to open it over");
            return;
        };
        if ui
            .handle
            .update(cx, |view, window, cx| view.open_settings(opening, window, cx))
            .is_err()
        {
            tracing::debug!("Settings: its window is gone");
        }
    }

    /// The model window the Settings popup is up over.
    #[cfg(test)]
    #[must_use]
    pub fn settings_host(&self, cx: &App) -> Option<Uuid> {
        self.settings_ui(cx).map(|(id, _)| id)
    }

    /// The window the Settings popup is up over.
    fn settings_ui(&self, cx: &App) -> Option<(Uuid, &WindowUi)> {
        self.shown.iter().find_map(|(id, ui)| {
            let up = ui.handle.read_with(cx, |view, _| view.settings().is_some());
            up.unwrap_or(false).then_some((*id, ui))
        })
    }

    /// Hands the Settings popup, while it is up, what `state` holds now.
    fn sync_settings(&self, state: &AppState, cx: &mut App) {
        let Some((_, ui)) = self.settings_ui(cx) else {
            return;
        };
        let snapshot = Snapshot::of(&state.core, state.applied_dark());
        if ui
            .handle
            .update(cx, |view, window, cx| view.sync_settings(snapshot, window, cx))
            .is_err()
        {
            tracing::debug!("the Settings popup's window is gone");
        }
    }

    /// Closes the windows of closed stores and opens one for each newly opened store; the ones opened, in order.
    fn open_and_close(&mut self, core: &mut Core, cx: &mut App) -> Vec<Uuid> {
        let library = core.library();
        let shown: HashSet<Uuid> = self.shown.keys().copied().collect();
        let plan = plan(&library.open_ids(), &shown, library.active_window_id());
        let known: Vec<Uuid> = library.windows().iter().map(|info| info.id).collect();
        for id in &plan.close {
            if let Some(ui) = self.shown.remove(id) {
                core.set_window_state(*id, None);
                let closed = ui.handle.update(cx, |view, window, cx| {
                    let frame = crate::window::frame_of(window);
                    view.close(window, cx);
                    frame
                });
                if let Ok(frame) = closed {
                    // upstream's `willClose` persists the final frame under the window's id
                    self.frames.save(*id, frame, &known);
                } else {
                    tracing::debug!(window = %id, "the closed window was gone already");
                }
            }
        }
        let frontmost_takes = core.frontmost_takes();
        let displays = crate::window::display_rects(cx);
        for id in &plan.build {
            let frame = self.frames.get(*id).map(|frame| frame.constrained(&displays));
            let Some(handle) = crate::window::open(*id, frontmost_takes, &self.app_id, &self.env, frame, cx) else {
                continue;
            };
            if let Ok(deck) = handle.update(cx, |view, _, _| view.deck().clone()) {
                self.shown.insert(*id, WindowUi { handle, deck });
            }
        }
        plan.build
    }

    /// What `window.list` reads of every window.
    fn report_states(&self, core: &mut Core, cx: &mut App) {
        for (id, ui) in &self.shown {
            if let Ok(state) = ui.handle.update(cx, |view, window, cx| view.window_state(window, cx)) {
                core.set_window_state(*id, Some(state));
            }
        }
    }

    /// Hands the panes the core granted their seeds.
    fn launch(&self, state: &mut AppState, cx: &App) {
        for grant in state.take_spawns() {
            if !self.shown.values().any(|ui| ui.deck.read(cx).launch(&grant)) {
                tracing::debug!(session = %grant.session, token = grant.token, "a granted pane has no terminal");
            }
        }
    }

    /// Shows the windows opened by this sync, the frontmost last, their terminals focused; the launch's own startup
    /// sequence ends on the first. A new window is the window manager's to focus: no activation is asked for it.
    fn present_built(&mut self, built: &[Uuid], cx: &mut App) {
        for id in built {
            let Some(ui) = self.shown.get(id) else { continue };
            let launched = self.launched.take();
            let presented = ui.handle.update(cx, |view, window, cx| {
                view.focus_terminal(window, cx);
                if let Some(activation) = &launched {
                    crate::activation::launched(window, cx, activation);
                }
            });
            if presented.is_err() {
                tracing::debug!(window = %id, "a new window is gone before it showed");
            }
        }
    }

    /// Applies one window command's request to its window; a window gone since asks nothing.
    fn apply(&self, request: WindowRequest, cx: &mut App) {
        let window = match request {
            WindowRequest::Raise(window)
            | WindowRequest::ToggleZoom(window)
            | WindowRequest::ToggleFullscreen(window)
            | WindowRequest::Resize { window, .. }
            | WindowRequest::Minimize { window, .. }
            | WindowRequest::Move { window, .. } => window,
        };
        if let Some(ui) = self.shown.get(&window) {
            let applied = ui.handle.update(cx, |view, window, cx| view.apply(request, window, cx));
            if applied.is_err() {
                tracing::debug!("a window request found its window gone");
            }
        }
    }

    /// Puts fresh terminals in place of the panes commands asked to replace (a lead takeover, a restart), before the
    /// windows render; each is reported back to the core, and one no window shows is dropped, logged. Whether a
    /// dropped one changed the model, which then needs another settle.
    fn replace_panes(&self, core: &mut Core, cx: &mut App) -> bool {
        let mut changed = false;
        for replacement in core.take_pane_replacements() {
            let window = core.library().window_holding_session(replacement.session);
            let replaced = window.and_then(|id| self.shown.get(&id)).is_some_and(|ui| {
                ui.handle
                    .update(cx, |view, window, cx| {
                        view.deck()
                            .clone()
                            .update(cx, |deck, cx| deck.replace_pane(core, &replacement, window, cx))
                    })
                    .unwrap_or(false)
            });
            if replaced {
                core.pane_replaced(&replacement);
            } else {
                tracing::warn!(session = %replacement.session, token = replacement.token, "no pane to replace");
                changed |= core.replacement_dropped(&replacement);
            }
        }
        changed
    }

    /// Rebuilds the ghostty config on the `dark` side of the desktop's appearance (upstream's
    /// `GhosttyApp.reloadConfig`) and feeds it to every terminal, their overlays (backgrounds) loaded on that side too;
    /// its diagnostic count, `None` when there is no config host (tests). With `keep_zoom` (an appearance flip) the
    /// terminals keep their live font sizes.
    fn reload_config(&self, dark: bool, keep_zoom: bool, cx: &mut App) -> Option<usize> {
        let config = self.config.as_ref()?;
        config.dark.set(dark);
        let app = AppConfig::load(&agterm_core::paths::ProcessEnv, config.resources.as_deref(), dark);
        *config.host.borrow_mut() =
            crate::session_host::launch_host(&app, config.resources.clone(), &agterm_core::paths::ProcessEnv);
        self.set_config(&app.terminal(), keep_zoom, cx);
        Some(app.diagnostics)
    }

    /// Every terminal takes `config` (over its own overlay), keeping its live font size with `keep_zoom`, and so does
    /// every terminal created from now on.
    pub fn set_config(&self, config: &TerminalConfig, keep_zoom: bool, cx: &mut App) {
        *self.env.config.borrow_mut() = Rc::new(config.clone());
        for ui in self.shown.values() {
            ui.deck.update(cx, |deck, cx| deck.apply_config(keep_zoom, cx));
        }
        self.quick.apply_config(&self.env, keep_zoom, cx);
        // the pages follow the terminals' colors (upstream's `refreshThemes` on `.agtermAppearanceChanged`)
        crate::html_overlay::refresh_themes(&crate::html_overlay::colors_of(config), cx);
    }

    /// Saves every open window's frame, keeping those of the library's `known` windows: the quit, which closes them all
    /// (upstream's windows closing at termination).
    pub fn save_frames(&mut self, known: &[Uuid], cx: &mut App) {
        let frames: Vec<(Uuid, crate::window_frames::Frame)> = self
            .shown
            .iter()
            .filter_map(|(id, ui)| {
                let frame = ui.handle.update(cx, |_, window, _| crate::window::frame_of(window));
                frame.ok().map(|frame| (*id, frame))
            })
            .collect();
        for (id, frame) in frames {
            self.frames.save(id, frame, known);
        }
    }

    /// Tears every terminal down, ending their programs: the quit path, after the quit save.
    pub fn teardown_all(&self, cx: &App) {
        let shared: Vec<_> = self
            .shown
            .values()
            .flat_map(|ui| {
                let deck = ui.deck.read(cx);
                deck.terminals()
                    .map(|pane| Rc::clone(pane.shared()))
                    .chain(deck.overlay_terminals().map(Rc::clone))
                    .collect::<Vec<_>>()
            })
            .chain(self.quick.shared().map(Rc::clone))
            .collect();
        if shared.is_empty() {
            return;
        }
        tracing::info!(count = shared.len(), "quit: tearing down the terminals");
        for terminal in shared {
            terminal.teardown();
        }
        tracing::info!("quit: every terminal is torn down");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(count: usize) -> Vec<Uuid> {
        (0..count).map(|_| Uuid::new_v4()).collect()
    }

    #[test]
    fn every_open_window_is_built_the_frontmost_last() {
        let open = ids(3);
        let plan = plan(&open, &HashSet::new(), Some(open[1]));
        assert_eq!(plan.build, vec![open[0], open[2], open[1]]);
        assert!(plan.close.is_empty());
    }

    #[test]
    fn shown_windows_stay_and_closed_ones_go() {
        let open = ids(3);
        let gone = Uuid::new_v4();
        let shown: HashSet<Uuid> = [open[0], open[1], gone].into_iter().collect();
        let plan = plan(&open, &shown, Some(open[0]));
        assert_eq!(plan.build, vec![open[2]], "only the newly opened one");
        assert_eq!(plan.close, vec![gone]);
    }

    /// A saved window set in `dir`: the seeded window and "work" open, "parked" closed, the seeded one frontmost; their
    /// ids in that order.
    fn saved_set(dir: &std::path::Path) -> [Uuid; 3] {
        let mut persistence = agterm_core::persist::LibraryPersistence::new(dir);
        let mut library = persistence.bootstrap("/home/u");
        let first = library.active_window_id().unwrap();
        let work = library.new_window(Some("work"));
        let parked = library.new_window(Some("parked"));
        assert!(persistence.save_all_checked(&library));
        library.close_window(parked);
        library.frontmost_window_id = Some(first);
        let _ = library.drain_effects();
        assert!(persistence.save_index(&library));
        [first, work, parked]
    }

    fn core_over(library: agterm_core::model::WindowLibrary) -> Core {
        Core::new(
            agterm_core::CoreConfig {
                app: agterm_proto::AppIdentity::from_recorded("1.2.3", None),
                capabilities: Vec::new(),
            },
            agterm_core::CoreState {
                library,
                ..agterm_core::CoreState::default()
            },
        )
    }

    /// A windows registry whose terminals never start (no process runs in this test).
    fn registry(state_dir: &std::path::Path, poster: crate::state::Poster) -> Windows {
        let launcher: crate::pane::Launcher = Rc::new(|_: &agterm_term::Launch| -> crate::terminal::Spawn {
            Box::new(|_| Err(agterm_term::PaneError::Closed))
        });
        let env = Rc::new(DeckEnv::new(
            crate::pane::PaneContext {
                socket: "/tmp/agterm-test.sock".to_owned(),
                program_version: "1.2.3".to_owned(),
            },
            launcher,
            TerminalConfig::default(),
            poster,
            crate::deck::Watermarks {
                state_dir: state_dir.to_path_buf(),
                config_with: Rc::new(|_: &str| TerminalConfig::default()),
            },
        ));
        Windows::new(env, "io.github.melonamin.agterm.test".to_owned(), None, None)
    }

    fn shown_ids(windows: &Windows) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = windows.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids
    }

    /// Upstream's `TitleProbeView`: a window opens at the frame saved when it last closed, and its close saves the one
    /// it has then; a window never closed opens at upstream's default size.
    #[gpui_kit::test]
    fn a_window_reopens_at_the_frame_it_closed_with(cx: &mut gpui_kit::TestAppContext) {
        use crate::window_frames::Frame;
        use gpui_kit::{px, size};

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::status_glyph::Motion::default());
        });
        let dir = tempfile::tempdir().unwrap();
        let [first, work, parked] = saved_set(dir.path());
        let saved = Frame {
            x: 120.0,
            y: 80.0,
            width: 1000.0,
            height: 700.0,
            maximized: false,
        };
        WindowFrames::load(dir.path()).save(parked, saved, &[first, work, parked]);
        let restored = agterm_core::persist::LibraryPersistence::new(dir.path()).bootstrap("/home/u");
        let mut state = AppState::new(core_over(restored));
        let mut windows = registry(dir.path(), crate::state::detached_poster());
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        let bounds = |windows: &Windows, id: Uuid, cx: &mut gpui_kit::TestAppContext| {
            let handle = windows.get(id).unwrap().handle;
            cx.update(|cx| handle.update(cx, |_, window, _| window.bounds()).unwrap())
        };
        assert_eq!(
            bounds(&windows, first, cx).size,
            size(px(900.), px(600.)),
            "upstream's default"
        );

        // the closed one comes back at its saved frame
        state.menu_list_activated(first, agterm_core::menu_lists::MenuListAction::OpenWindow(parked));
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        let reopened = bounds(&windows, parked, cx);
        assert_eq!(reopened.size, size(px(1000.), px(700.)));
        assert_eq!(reopened.origin, gpui_kit::point(px(120.), px(80.)));

        // closing one saves the frame it has now, for its next opening
        let handle = windows.get(work).unwrap().handle;
        cx.update(|cx| handle.update(cx, |_, window, _| window.resize(size(px(820.), px(510.)))))
            .unwrap();
        state.close_confirmed(work);
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        let frame = WindowFrames::load(dir.path()).get(work).expect("saved at its close");
        assert_eq!((frame.width, frame.height), (820.0, 510.0));
        assert!(!frame.maximized);
        // the quit saves every open one
        let known = [first, work, parked];
        cx.update(|cx| windows.save_frames(&known, cx));
        assert!(WindowFrames::load(dir.path()).get(first).is_some());
        cx.update(|cx| windows.teardown_all(cx));
    }

    #[gpui_kit::test]
    fn a_saved_window_set_reopens_its_open_windows_and_keeps_the_closed_one(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            // the desktop's animation switch is not followed here: no portal is asked
            cx.set_global(crate::status_glyph::Motion::default());
        });
        let dir = tempfile::tempdir().unwrap();
        let [first, work, parked] = saved_set(dir.path());
        // the next launch's bootstrap from the same state dir, as `app.rs` `load_state` runs it
        let restored = agterm_core::persist::LibraryPersistence::new(dir.path()).bootstrap("/home/u");
        assert_eq!(restored.windows().len(), 3, "the library keeps every window");
        assert_eq!(restored.active_window_id(), Some(first), "frontmost as saved");
        let mut state = AppState::new(core_over(restored));
        let mut windows = registry(dir.path(), crate::state::detached_poster());

        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        let mut open = vec![first, work];
        open.sort_unstable();
        assert_eq!(
            shown_ids(&windows),
            open,
            "one window per open store, the closed one not opened"
        );
        assert!(windows.get(parked).is_none());
        for id in [first, work] {
            assert!(
                state.core.window_state(id).is_some(),
                "window.list reads every shown window"
            );
        }
        let title = windows
            .get(work)
            .unwrap()
            .handle
            .read_with(cx, |view, _| view.title().to_owned());
        assert_eq!(title.unwrap(), "u — work", "titled after its session and its name");

        // the closed one reopens from its file through File ▸ Open Window
        state.menu_list_activated(first, agterm_core::menu_lists::MenuListAction::OpenWindow(parked));
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        assert!(windows.get(parked).is_some(), "reopened");
        assert_eq!(state.core.library().active_window_id(), Some(parked), "and frontmost");

        // the user closes one: its window goes, the others stay
        state.close_confirmed(work);
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        let mut left = vec![first, parked];
        left.sort_unstable();
        assert_eq!(shown_ids(&windows), left);
        assert!(!state.core.library().is_open(work));
        assert!(!state.take_frontend_work().quit, "windows are still open");
        cx.update(|cx| windows.teardown_all(cx));
    }

    /// One Settings popup in the app: it opens over the window asked for (the frontmost one when that is not on
    /// screen), asking again from another window brings its own window up instead of opening a second, it follows the
    /// core through the windows' sync, and it goes with its window.
    #[gpui_kit::test]
    fn settings_is_one_popup_over_the_window_it_was_asked_from(cx: &mut gpui_kit::TestAppContext) {
        use crate::settings::Opening;

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::status_glyph::Motion::default());
        });
        let dir = tempfile::tempdir().unwrap();
        let [first, work, _] = saved_set(dir.path());
        let restored = agterm_core::persist::LibraryPersistence::new(dir.path()).bootstrap("/home/u");
        let mut state = AppState::new(core_over(restored));
        let mut windows = registry(dir.path(), crate::state::detached_poster());
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        let present = |windows: &Windows, state: &AppState, host, cx: &mut gpui_kit::TestAppContext| {
            let frontmost = state.core.library().active_window_id();
            cx.update(|cx| windows.present_settings(Opening::of(state), host, frontmost, cx));
            cx.run_until_parked();
        };
        let popups = |windows: &Windows, cx: &mut gpui_kit::TestAppContext| {
            cx.update(|cx| {
                windows
                    .iter()
                    .filter(|(_, ui)| {
                        ui.handle
                            .read_with(cx, |view, _| view.settings().is_some())
                            .unwrap_or(false)
                    })
                    .count()
            })
        };
        present(&windows, &state, work, cx);
        assert_eq!(
            cx.update(|cx| windows.settings_host(cx)),
            Some(work),
            "over the menu's window"
        );
        present(&windows, &state, first, cx);
        assert_eq!(
            cx.update(|cx| windows.settings_host(cx)),
            Some(work),
            "the one popup stays"
        );
        assert_eq!(popups(&windows, cx), 1, "never a second");
        // the windows' sync hands it what the core holds
        state
            .core
            .change_settings(agterm_core::settings_model::SettingsChange::General(
                agterm_core::settings_model::GeneralChange::ConfirmCloseSession(true),
            ));
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        let shown = cx.update(|cx| {
            windows.get(work).unwrap().handle.read_with(cx, |view, cx| {
                view.settings()
                    .map(|popup| popup.read(cx).snapshot().settings.confirm_close_session)
            })
        });
        assert_eq!(shown.unwrap(), Some(Some(true)));
        // its window closing takes it down; a window not on screen falls back to the frontmost
        state.close_confirmed(work);
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| windows.settings_host(cx)), None, "gone with its window");
        present(&windows, &state, work, cx);
        let frontmost = state.core.library().active_window_id();
        assert_eq!(cx.update(|cx| windows.settings_host(cx)), frontmost);
        assert_eq!(frontmost, Some(first));
        // closed, it opens again over the window asked for
        let handle = windows.get(first).unwrap().handle;
        cx.update(|cx| handle.update(cx, crate::window::WindowView::close_settings))
            .unwrap();
        assert_eq!(cx.update(|cx| windows.settings_host(cx)), None);
        present(&windows, &state, first, cx);
        assert_eq!(cx.update(|cx| windows.settings_host(cx)), Some(first));
        cx.update(|cx| windows.teardown_all(cx));
    }

    /// Settings… asked again while a picker is up over the popup brings the popup's window up with the picker still
    /// holding the keyboard: the popup under it does not take it.
    #[gpui_kit::test]
    fn settings_asked_again_leaves_the_keyboard_on_a_picker_over_the_popup(cx: &mut gpui_kit::TestAppContext) {
        use crate::settings::Opening;

        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::status_glyph::Motion::default());
        });
        let dir = tempfile::tempdir().unwrap();
        let [first, work, _] = saved_set(dir.path());
        let restored = agterm_core::persist::LibraryPersistence::new(dir.path()).bootstrap("/home/u");
        let mut state = AppState::new(core_over(restored));
        let mut windows = registry(dir.path(), crate::state::detached_poster());
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        let present = |windows: &Windows, state: &AppState, host, cx: &mut gpui_kit::TestAppContext| {
            let frontmost = state.core.library().active_window_id();
            cx.update(|cx| windows.present_settings(Opening::of(state), host, frontmost, cx));
            cx.run_until_parked();
        };
        // who holds the keyboard of `first`'s window: (the picker, the popup)
        let held = |windows: &Windows, cx: &mut gpui_kit::TestAppContext| {
            let handle = windows.get(first).unwrap().handle;
            cx.update(|cx| {
                handle
                    .update(cx, |view, window, cx| {
                        let picker = view
                            .picker()
                            .is_some_and(|picker| picker.read(cx).focus_handle(cx).is_focused(window));
                        let popup = view
                            .settings()
                            .is_some_and(|popup| popup.read(cx).holds_keyboard(window, cx));
                        (picker, popup)
                    })
                    .unwrap()
            })
        };
        present(&windows, &state, first, cx);
        assert_eq!(cx.update(|cx| windows.settings_host(cx)), Some(first));
        assert_eq!(held(&windows, cx), (false, true), "the popup holds the keyboard");
        let item = |id: &str| agterm_proto::PickItem {
            id: id.to_owned(),
            label: id.to_owned(),
            subtitle: None,
        };
        let response = state.core.dispatch(&agterm_proto::Request {
            cmd: agterm_proto::Command::Pick(agterm_proto::command::PickCmd::Open),
            target: None,
            args: Some(agterm_proto::Args {
                items: Some(vec![item("alpha"), item("beta")]),
                ..agterm_proto::Args::default()
            }),
        });
        assert!(response.ok, "{response:?}");
        state.settle();
        cx.update(|cx| {
            let _ = windows.sync(&mut state, cx);
        });
        cx.run_until_parked();
        assert_eq!(held(&windows, cx), (true, false), "the picker over the popup holds it");
        // Settings… again, from the other window's menu
        present(&windows, &state, work, cx);
        assert_eq!(cx.update(|cx| windows.settings_host(cx)), Some(first), "the one popup");
        assert_eq!(held(&windows, cx), (true, false), "the picker keeps the keyboard");
        cx.update(|cx| windows.teardown_all(cx));
    }

    #[test]
    fn no_open_window_closes_them_all() {
        let shown: HashSet<Uuid> = ids(2).into_iter().collect();
        let mut expected: Vec<Uuid> = shown.iter().copied().collect();
        expected.sort_unstable();
        assert_eq!(
            plan(&[], &shown, None),
            WindowsPlan {
                close: expected,
                build: Vec::new()
            }
        );
    }
}
