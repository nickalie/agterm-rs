//! The detail deck: every session of the shown window holds one page with its terminal, all mounted at once so each
//! spawns its shell at startup, and only the selected one visible, hit-testable and focused (upstream's eager deck,
//! `WindowContentView+Detail.swift` `detailPane`, and `libghostty.md` > Resources and lifecycle).
//!
//! A page is created once and never re-parented: a live `GtkGLArea` must stay realized, as unrealizing it frees its
//! surface. Switching sessions is a visibility flip: a hidden page sits at opacity 0, not targetable, its terminal
//! unfocusable and occluded, but mapped, so its surface is still created and its shell runs (the conformance
//! harness's `wait_mounted` waits for a `noSelect` session's `realized` too). Sessions a soft close holds out of the
//! tree keep their pages, hidden, until the close is final. A page goes once its session is gone; closing the session
//! already tore its terminal down.
//!
//! The plan of what to add, remove and show is a pure function of the model ([`plan`]), so it is testable without a
//! display.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agterm_core::Core;
use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::{AppStore, WindowLibrary};
use agterm_core::pane_env::{self, PaneEnv, SessionPane, StatusPane};
use gtk::prelude::*;
use uuid::Uuid;

use crate::ghostty_app;
use crate::pane::PaneSurface;
use crate::state::{self, AppState, PaneReport, Poster};
use crate::surface::{SurfaceSpec, TerminalSurface};

/// What upstream's deck shows with no session selected.
const NO_SESSION: &str = "No session selected";

/// The terminal a new page runs: its session's primary pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpec {
    pub window: Uuid,
    pub workspace: Uuid,
    pub session: Uuid,
    /// The pane token, `AGTERM_PANE_ID` (upstream's `paneIdentity.uuidString`).
    pub pane_identity: Uuid,
    pub cwd: String,
    /// `session.new --command`, run instead of the login shell.
    pub command: Option<String>,
    /// Hold the pane after the command exits.
    pub wait: bool,
}

impl PageSpec {
    /// The primary pane of `session` in `store`, `None` when the session is not in its tree.
    #[must_use]
    pub fn primary(window: Uuid, store: &AppStore, session: Uuid) -> Option<Self> {
        let workspace = store.workspace_for_session(session)?.id();
        let session = store.session(session)?;
        Some(Self {
            window,
            workspace,
            session: session.id(),
            pane_identity: session.pane_identity,
            cwd: session.effective_cwd().to_owned(),
            command: session.launch.command.clone(),
            wait: session.launch.wait,
        })
    }

    /// The pane token baked into the shell and reported by the session's surface slot.
    #[must_use]
    pub fn token(&self) -> String {
        pane_env::uuid_string(self.pane_identity)
    }

    /// The primary pane's environment: the session, window and workspace ids, `AGTERM_PANE=left` and the pane token.
    #[must_use]
    pub fn env(&self, context: &PaneContext) -> PaneEnv {
        let token = self.token();
        pane_env::session(&SessionPane {
            session: self.session,
            window: Some(self.window),
            workspace: Some(self.workspace),
            socket_path: &context.socket,
            program_version: &context.program_version,
            pane: Some(StatusPane::Left),
            pane_token: Some(&token),
        })
    }

    /// What the terminal runs.
    #[must_use]
    pub fn surface_spec(&self, context: &PaneContext) -> SurfaceSpec {
        SurfaceSpec {
            working_directory: Some(PathBuf::from(&self.cwd)),
            command: self.command.clone(),
            env: self.env(context).into_iter().collect(),
            wait_after_command: self.wait,
        }
    }
}

/// What every shell of this instance is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneContext {
    /// `AGTERM_SOCKET`: the owned socket, or `<socket>.unavailable` when another instance owns it.
    pub socket: String,
    /// `TERM_PROGRAM_VERSION`.
    pub program_version: String,
}

/// One reconcile of the deck against the model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeckPlan {
    /// Sessions without a page yet, in tree order.
    pub add: Vec<PageSpec>,
    /// Pages whose session is gone (closed, a soft close made final, moved out of the window).
    pub remove: Vec<Uuid>,
    /// The page to show: the selected session's, `None` when no session is selected.
    pub shown: Option<Uuid>,
}

/// What the deck of `window` must add, remove and show, given the sessions it has `mounted` pages for. Every session
/// of the window's tree gets a page; a session a pending soft close holds keeps its page; the rest go. With `window`
/// `None` or not open, every page goes.
#[must_use]
pub fn plan(library: &WindowLibrary, window: Option<Uuid>, mounted: &HashSet<Uuid>) -> DeckPlan {
    let Some((window, store)) = window.and_then(|id| library.store(id).map(|store| (id, store))) else {
        let mut remove: Vec<Uuid> = mounted.iter().copied().collect();
        remove.sort_unstable();
        return DeckPlan {
            remove,
            ..DeckPlan::default()
        };
    };
    plan_store(window, store, &store.pending_held_session_ids(), mounted)
}

/// [`plan`] for the open window `window`, whose pending soft closes hold the sessions `held`.
fn plan_store(window: Uuid, store: &AppStore, held: &HashSet<Uuid>, mounted: &HashSet<Uuid>) -> DeckPlan {
    let add = store
        .sessions()
        .filter(|session| !mounted.contains(&session.id()))
        .filter_map(|session| PageSpec::primary(window, store, session.id()))
        .collect();
    let mut remove: Vec<Uuid> = mounted
        .iter()
        .copied()
        .filter(|id| store.session(*id).is_none() && !held.contains(id))
        .collect();
    remove.sort_unstable();
    DeckPlan {
        add,
        remove,
        shown: store.selected_session_id().filter(|id| store.session(*id).is_some()),
    }
}

/// One session's page: a container for its panes (the split comes with Task 16) and the primary terminal.
struct Page {
    widget: gtk::Box,
    terminal: TerminalSurface,
}

/// The deck widget: the "No session selected" placeholder under one overlay page per session.
pub struct Deck {
    overlay: gtk::Overlay,
    placeholder: gtk::Label,
    pages: HashMap<Uuid, Page>,
    shown: Option<Uuid>,
}

impl Deck {
    #[must_use]
    pub fn new() -> Self {
        let placeholder = gtk::Label::builder()
            .label(NO_SESSION)
            .hexpand(true)
            .vexpand(true)
            .build();
        placeholder.add_css_class("dim-label");
        let overlay = gtk::Overlay::builder().hexpand(true).vexpand(true).build();
        overlay.set_child(Some(&placeholder));
        Self {
            overlay,
            placeholder,
            pages: HashMap::new(),
            shown: None,
        }
    }

    /// The widget to place in the window.
    #[must_use]
    pub fn widget(&self) -> &gtk::Overlay {
        &self.overlay
    }

    /// The shown page's terminal.
    #[must_use]
    pub fn shown_terminal(&self) -> Option<&TerminalSurface> {
        self.shown.and_then(|id| self.pages.get(&id)).map(|page| &page.terminal)
    }

    /// Brings the deck in line with `window`'s sessions: pages for new sessions (their terminals installed in the
    /// sessions' surface slots), gone sessions' pages removed, the selected session's page shown and focused when
    /// the selection changed. While the window is zoomed to one of `zoomed`'s terminals, that session's page shows
    /// instead, selected or not (upstream's zoom layer; a terminal is never reparented, so its own page fills).
    pub fn reconcile(
        &mut self,
        core: &mut Core,
        window: Option<Uuid>,
        zoomed: Option<Uuid>,
        context: &PaneContext,
        poster: &Poster,
    ) {
        let mounted: HashSet<Uuid> = self.pages.keys().copied().collect();
        let plan = plan(core.library(), window, &mounted);
        for id in &plan.remove {
            if let Some(page) = self.pages.remove(id) {
                // the session's close tore the terminal down already; this only drops the widget
                page.terminal.teardown();
                self.overlay.remove_overlay(&page.widget);
            }
        }
        for spec in &plan.add {
            self.add_page(core, spec, context, poster);
        }
        let zoomed = zoomed.filter(|id| self.pages.contains_key(id));
        self.show(zoomed.or(plan.shown));
    }

    fn add_page(&mut self, core: &mut Core, spec: &PageSpec, context: &PaneContext, poster: &Poster) {
        let terminal = TerminalSurface::new(spec.surface_spec(context));
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(terminal.widget());
        set_page_visible(&widget, &terminal, false);
        self.overlay.add_overlay(&widget);
        connect_terminal(&terminal, spec, poster);
        let slot = core
            .library_mut()
            .store_mut(spec.window)
            .and_then(|store| store.session_mut(spec.session))
            .map(|session| &mut session.surface);
        if let Some(slot) = slot
            && let Some(previous) = slot.replace(Box::new(PaneSurface::new(terminal.clone(), spec.token())))
        {
            tracing::error!(session = %spec.session, "a session got a second primary terminal");
            previous.teardown();
        }
        self.pages.insert(spec.session, Page { widget, terminal });
    }

    /// Shows the page of `shown` alone; a change of the shown page moves keyboard focus to it (upstream focuses the
    /// active session after a select or a create), and with none shown the focus leaves the hidden terminals.
    fn show(&mut self, shown: Option<Uuid>) {
        for (id, page) in &self.pages {
            let visible = Some(*id) == shown;
            if page.terminal.is_deck_visible() != visible {
                set_page_visible(&page.widget, &page.terminal, visible);
            }
        }
        self.placeholder.set_visible(shown.is_none());
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        match self.shown_terminal() {
            Some(terminal) => terminal.grab_focus(),
            None => {
                if let Some(root) = self.overlay.root() {
                    root.set_focus(None::<&gtk::Widget>);
                }
            }
        }
    }
}

impl Default for Deck {
    fn default() -> Self {
        Self::new()
    }
}

/// The one visibility switch of a page (see the module docs): a hidden page stays mapped and allocated so its
/// surface exists, but is not drawn, hit-tested or focusable, and its terminal is occluded.
fn set_page_visible(widget: &gtk::Box, terminal: &TerminalSurface, visible: bool) {
    widget.set_opacity(if visible { 1.0 } else { 0.0 });
    widget.set_can_target(visible);
    widget.set_can_focus(visible);
    terminal.set_deck_visible(visible);
}

/// Routes the terminal's events to the model as main-loop tasks: its shell exiting closes the session (upstream's
/// `handlePaneExit` without a split), its cwd and title reports land on the session, its search reports on the
/// session's search (upstream's `wireSearchCallbacks`), and a clicked link opens through the link policy as a click
/// in this session's pane, under the link-open setting.
fn connect_terminal(terminal: &TerminalSurface, spec: &PageSpec, poster: &Poster) {
    let (session, token) = (spec.session, spec.token());
    let posting = poster.clone();
    let exit_token = token.clone();
    terminal.connect_exit(move || {
        state::post(&posting, move |state: &mut AppState| {
            state.close_exited_session(session, &exit_token);
        });
    });
    let posting = poster.clone();
    terminal.connect_search(move |report| {
        let (report, token) = (report.clone(), token.clone());
        state::post(&posting, move |state: &mut AppState| {
            state.core.report_search(session, &token, report);
            state.settle();
        });
    });
    let posting = poster.clone();
    terminal.connect_open_url(move |url| {
        let url = url.to_owned();
        state::post(&posting, move |state: &mut AppState| {
            let mode = state.core.settings().effective_link_open_mode();
            ghostty_app::follow_link(&url, mode, ClickOrigin::Pane(session));
        });
    });
    let posting = poster.clone();
    terminal.connect_pwd(move |pwd| {
        let report = PaneReport::Pwd(pwd.to_owned());
        state::post(&posting, move |state: &mut AppState| {
            state.apply_report(session, &report);
        });
    });
    let posting = poster.clone();
    terminal.connect_title(move |title| {
        let report = PaneReport::Title(title.to_owned());
        state::post(&posting, move |state: &mut AppState| {
            state.apply_report(session, &report);
        });
    });
}

#[cfg(test)]
mod tests {
    use agterm_core::model::NewSession;

    use super::*;

    fn context() -> PaneContext {
        PaneContext {
            socket: "/tmp/a.sock".to_owned(),
            program_version: "1.2.3".to_owned(),
        }
    }

    /// A window of one workspace holding three sessions, the first selected.
    fn three_sessions() -> (WindowLibrary, Uuid, Vec<Uuid>) {
        let mut library = WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        let store = library.store_mut(window).unwrap();
        let workspace = store.workspaces()[0].id();
        let first = store.sessions().next().unwrap().id();
        let second = store.add_session(workspace, NewSession::at_cwd("/srv")).unwrap();
        let third = store
            .add_session(
                workspace,
                NewSession {
                    command: Some("htop".to_owned()),
                    wait: true,
                    select: false,
                    ..NewSession::at_cwd("/var")
                },
            )
            .unwrap();
        store.select_session(Some(first));
        (library, window, vec![first, second, third])
    }

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
    fn a_session_selected_later_shows_without_a_new_page() {
        let (mut library, window, ids) = three_sessions();
        let mounted: HashSet<Uuid> = ids.iter().copied().collect();
        library.store_mut(window).unwrap().select_session(Some(ids[2]));
        let plan = plan(&library, Some(window), &mounted);
        assert!(plan.add.is_empty() && plan.remove.is_empty());
        assert_eq!(plan.shown, Some(ids[2]));
    }

    #[test]
    fn no_selection_shows_no_page() {
        let (mut library, window, ids) = three_sessions();
        library.store_mut(window).unwrap().select_session(None);
        let plan = plan(&library, Some(window), &ids.iter().copied().collect());
        assert_eq!(plan.shown, None);
    }

    #[test]
    fn a_closed_window_or_none_drops_every_page() {
        let (mut library, window, ids) = three_sessions();
        let mounted: HashSet<Uuid> = ids.iter().copied().collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(plan(&library, None, &mounted).remove, sorted);
        library.new_window(None);
        library.close_window(window);
        let plan = plan(&library, Some(window), &mounted);
        assert_eq!(plan.remove, sorted);
        assert!(plan.add.is_empty());
        assert_eq!(plan.shown, None);
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
    fn the_page_env_names_the_session_its_window_workspace_and_pane() {
        let (library, window, ids) = three_sessions();
        let store = library.store(window).unwrap();
        let workspace = store.workspaces()[0].id();
        for id in ids {
            let spec = PageSpec::primary(window, store, id).unwrap();
            let session = store.session(id).unwrap();
            let env = spec.env(&context());
            assert_eq!(env["AGTERM_SESSION_ID"], pane_env::uuid_string(id));
            assert_eq!(env["AGTERM_WINDOW_ID"], pane_env::uuid_string(window));
            assert_eq!(env["AGTERM_WORKSPACE_ID"], pane_env::uuid_string(workspace));
            assert_eq!(env["AGTERM_PANE"], "left");
            assert_eq!(env["AGTERM_PANE_ID"], pane_env::uuid_string(session.pane_identity));
            assert_eq!(spec.token(), env["AGTERM_PANE_ID"]);
            assert_eq!(env["AGTERM_SOCKET"], "/tmp/a.sock");
            assert_eq!(env["TERM_PROGRAM"], "agterm");
            assert_eq!(env["TERM_PROGRAM_VERSION"], "1.2.3");
            let surface = spec.surface_spec(&context());
            assert_eq!(surface.working_directory, Some(PathBuf::from(session.effective_cwd())));
            assert_eq!(surface.command, session.launch.command);
            assert_eq!(surface.wait_after_command, session.launch.wait);
        }
    }

    #[test]
    fn a_session_outside_the_tree_has_no_page_spec() {
        let (library, window, _) = three_sessions();
        assert_eq!(
            PageSpec::primary(window, library.store(window).unwrap(), Uuid::new_v4()),
            None
        );
    }
}
