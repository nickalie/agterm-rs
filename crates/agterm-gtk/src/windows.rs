//! The on-screen windows, one GTK window per open model window (upstream's `WindowRegistry.swift`): built when a store
//! opens, closed when it closes, each over its own store with its own deck and sidebar.
//!
//! The model's open set drives it. After every commit [`Windows::sync`] closes the windows of closed stores, builds and
//! presents one for each newly opened store (the frontmost last, so it ends up on top), renders every window, then
//! applies what the window commands asked of them ([`WindowRequest`]). A live terminal never moves between windows: a
//! session's page lives in its own window's deck, created there and torn down there, so no `GtkGLArea` is ever
//! re-parented (upstream has no cross-window session move either: "cross-window session drag is out of scope",
//! `windows.md`). Once no window is open the app quits, as upstream's `applicationShouldTerminateAfterLastWindowClosed`
//! keys termination off the model's open set.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use agterm_core::{Core, WindowRequest};
use uuid::Uuid;

use crate::deck::PaneContext;
use crate::state::Poster;
use crate::window::WindowUi;

/// One reconcile of the on-screen windows against the open set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowsPlan {
    /// Windows whose store closed (or was deleted).
    pub close: Vec<Uuid>,
    /// Open windows without a GTK window yet, in the order to present them: window order, the frontmost last.
    pub build: Vec<Uuid>,
}

/// What [`Windows::sync`] closes and builds, given the `open` window ids in window order, the windows `shown`, and the
/// `frontmost` one.
#[must_use]
pub fn plan(open: &[Uuid], shown: &HashSet<Uuid>, frontmost: Option<Uuid>) -> WindowsPlan {
    let mut close: Vec<Uuid> = shown.iter().copied().filter(|id| !open.contains(id)).collect();
    close.sort_unstable();
    let mut build: Vec<Uuid> = open.iter().copied().filter(|id| !shown.contains(id)).collect();
    if let Some(index) = build.iter().position(|id| Some(*id) == frontmost) {
        let front = build.remove(index);
        build.push(front);
    }
    WindowsPlan { close, build }
}

/// The GTK windows by model window id.
pub struct Windows {
    app: adw::Application,
    context: PaneContext,
    menu: crate::app_menu::AppMenu,
    shown: HashMap<Uuid, WindowUi>,
}

impl Windows {
    #[must_use]
    pub fn new(app: adw::Application, context: PaneContext, menu: crate::app_menu::AppMenu) -> Self {
        Self {
            app,
            context,
            menu,
            shown: HashMap::new(),
        }
    }

    /// The application the windows belong to.
    #[must_use]
    pub fn app(&self) -> &adw::Application {
        &self.app
    }

    /// The app menu every window shows.
    #[must_use]
    pub fn menu(&self) -> &crate::app_menu::AppMenu {
        &self.menu
    }

    /// What every shell of this instance is told about it.
    #[must_use]
    pub fn context(&self) -> &PaneContext {
        &self.context
    }

    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<&WindowUi> {
        self.shown.get(&id)
    }

    pub fn get_mut(&mut self, id: Uuid) -> Option<&mut WindowUi> {
        self.shown.get_mut(&id)
    }

    /// Raises the frontmost window.
    pub fn present_frontmost(&self, core: &Core) {
        if let Some(ui) = core.library().active_window_id().and_then(|id| self.shown.get(&id)) {
            ui.present();
        }
    }

    /// Puts fresh terminals in place of the panes commands asked to replace (a lead takeover, a restart), before the
    /// windows render; a pane no window shows is skipped, logged.
    pub fn replace_panes(&mut self, core: &mut Core, poster: &Poster) {
        for replacement in core.take_pane_replacements() {
            let window = core.library().window_holding_session(replacement.session);
            let replaced = window
                .and_then(|id| self.shown.get_mut(&id))
                .is_some_and(|ui| ui.replace_pane(core, &replacement, poster));
            if !replaced {
                tracing::warn!(session = %replacement.session, token = replacement.token, "no pane to replace");
            }
        }
    }

    /// Brings the windows in line with the model (module docs), then reports every window's state to the core.
    pub fn sync(&mut self, core: &mut Core, poster: &Poster) {
        // a dashboard follows the model before anything renders it
        core.reconcile_dashboards();
        let library = core.library();
        let shown: HashSet<Uuid> = self.shown.keys().copied().collect();
        let plan = plan(&library.open_ids(), &shown, library.active_window_id());
        for id in &plan.close {
            if let Some(ui) = self.shown.remove(id) {
                core.set_window_state(*id, None);
                ui.close();
            }
        }
        for id in &plan.build {
            let ui = WindowUi::build(&self.app, *id, self.context.clone(), &self.menu, poster);
            self.shown.insert(*id, ui);
        }
        for ui in self.shown.values_mut() {
            ui.sync(core, poster);
        }
        for id in &plan.build {
            if let Some(ui) = self.shown.get(id) {
                ui.present();
            }
        }
        for request in core.take_window_requests() {
            self.apply(request);
        }
        for (id, ui) in &self.shown {
            core.set_window_state(*id, Some(ui.window_state()));
        }
        if self.shown.is_empty() {
            tracing::info!("no window is open: quitting");
            self.app.quit();
        }
    }

    /// Applies one window command's request to its window; a window gone since asks nothing.
    fn apply(&self, request: WindowRequest) {
        let window = match request {
            WindowRequest::Raise(window)
            | WindowRequest::ToggleZoom(window)
            | WindowRequest::ToggleFullscreen(window)
            | WindowRequest::Resize { window, .. }
            | WindowRequest::Minimize { window, .. }
            | WindowRequest::Move { window, .. } => window,
        };
        if let Some(ui) = self.shown.get(&window) {
            ui.apply(request);
        }
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
