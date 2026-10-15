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
use agterm_term::Host;
use gpui_kit::{App, Entity, WindowHandle};
use uuid::Uuid;

use crate::deck::{Deck, DeckEnv};
use crate::instance::Activation;
use crate::session_host::AppConfig;
use crate::state::AppState;
use crate::terminal::TerminalConfig;
use crate::window::WindowView;

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
}

impl Windows {
    #[must_use]
    pub fn new(env: Rc<DeckEnv>, app_id: String, config: Option<ConfigHost>, launched: Option<Activation>) -> Self {
        Self {
            env,
            app_id,
            config,
            shown: HashMap::new(),
            launched,
        }
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
            let count = self.reload_config(reload.dark, cx);
            state.config_applied(reload.dark);
            if reload.asked {
                state.core.report_config_reload(count);
            }
        }
        // after any reload, so the chrome takes the colors the terminals now render; before the windows render, so a
        // new window's first frame has them
        crate::chrome::sync(&state.core, &self.env.config.borrow(), cx);
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
        self.report_states(&mut state.core, cx);
        state.core.clear_pane_focus_requests();
        self.launch(state, cx);
        if self.shown.is_empty() {
            tracing::info!("no window is open: quitting");
            state.request_quit();
        }
        resettle
    }

    /// Closes the windows of closed stores and opens one for each newly opened store; the ones opened, in order.
    fn open_and_close(&mut self, core: &mut Core, cx: &mut App) -> Vec<Uuid> {
        let library = core.library();
        let shown: HashSet<Uuid> = self.shown.keys().copied().collect();
        let plan = plan(&library.open_ids(), &shown, library.active_window_id());
        for id in &plan.close {
            if let Some(ui) = self.shown.remove(id) {
                core.set_window_state(*id, None);
                if ui.handle.update(cx, |view, window, _| view.close(window)).is_err() {
                    tracing::debug!(window = %id, "the closed window was gone already");
                }
            }
        }
        let frontmost_takes = core.frontmost_takes();
        for id in &plan.build {
            let Some(handle) = crate::window::open(*id, frontmost_takes, &self.app_id, &self.env, cx) else {
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
    /// its diagnostic count, `None` when there is no config host (tests).
    fn reload_config(&self, dark: bool, cx: &mut App) -> Option<usize> {
        let config = self.config.as_ref()?;
        config.dark.set(dark);
        let app = AppConfig::load(&agterm_core::paths::ProcessEnv, config.resources.as_deref(), dark);
        *config.host.borrow_mut() =
            crate::session_host::launch_host(&app, config.resources.clone(), &agterm_core::paths::ProcessEnv);
        self.set_config(&app.terminal(), cx);
        Some(app.diagnostics)
    }

    /// Every terminal takes `config` (over its own overlay), and so does every terminal created from now on.
    pub fn set_config(&self, config: &TerminalConfig, cx: &mut App) {
        *self.env.config.borrow_mut() = Rc::new(config.clone());
        for ui in self.shown.values() {
            ui.deck.update(cx, |deck, cx| deck.apply_config(cx));
        }
    }

    /// Tears every terminal down, ending their programs: the quit path, after the quit save.
    pub fn teardown_all(&self, cx: &App) {
        let shared: Vec<_> = self
            .shown
            .values()
            .flat_map(|ui| {
                ui.deck
                    .read(cx)
                    .terminals()
                    .map(|pane| Rc::clone(pane.shared()))
                    .collect::<Vec<_>>()
            })
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
