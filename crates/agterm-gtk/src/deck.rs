//! The detail deck: every session of the shown window holds one page with its terminals, all mounted at once so each
//! spawns its shell at startup, and only the selected one visible, hit-testable and focused (upstream's eager deck,
//! `WindowContentView+Detail.swift` `detailPane`, and `libghostty.md` > Resources and lifecycle).
//!
//! A page is created once and never re-parented: a live `GtkGLArea` must stay realized, as unrealizing it frees its
//! surface. Switching sessions is a visibility flip: a hidden page sits at opacity 0, not targetable, its terminals
//! unfocusable and occluded, but mapped, so their surfaces are still created and their shells run (the conformance
//! harness's `wait_mounted` waits for a `noSelect` session's `realized` too). Sessions a soft close holds out of the
//! tree keep their pages, hidden, until the close is final. A page goes once its session is gone; closing the session
//! already tore its terminals down. Inside a page the session's panes follow [`crate::split`].
//!
//! The model moves keyboard focus only by what it says should hold it: the deck grabs focus for the shown page's
//! focused pane whenever that pane (or the shown page) differs from what it last focused, which is how selecting a
//! session, `session.focus`, a swap or the scratch move the input. A terminal the user focuses reports back through
//! [`AppState::pane_focused`], from a task that first checks the focus is still there.
//!
//! While the window's dashboard is open every member session's page shows at once, each laying its member panes out
//! at their grid cells over an opaque backdrop, under the grid's chrome, which holds the keyboard ([`crate::dashboard`]).
//!
//! The plan of what to add, remove and show is a pure function of the model ([`plan`]), so it is testable without a
//! display.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::pane::SPLIT_RATIO_DEFAULT;
use agterm_core::model::{AppStore, Session, WindowLibrary, Workspace};
use agterm_core::pane_env::{self, PaneEnv, SessionPane, StatusPane};
use agterm_core::zoom::{SurfaceId, ZoomSurface};
use agterm_core::{Core, PaneReport};
use gtk::prelude::*;
use uuid::Uuid;

use crate::dashboard::{self, Chrome, FontOverrides, GridModel};
use crate::ghostty_app;
use crate::pane::PaneSurface;
use crate::split::{self, PanePlan, PaneView};
use crate::state::{self, AppState, Poster};
use crate::surface::{self, SurfaceSpec, TerminalSurface};

/// What upstream's deck shows with no session selected.
const NO_SESSION: &str = "No session selected";

/// A terminal a page runs: one of its session's panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpec {
    pub window: Uuid,
    pub workspace: Uuid,
    pub session: Uuid,
    /// `AGTERM_PANE`.
    pub pane: StatusPane,
    /// The pane token, `AGTERM_PANE_ID` (upstream's `paneIdentity.uuidString` and its split twin; a fresh one per
    /// scratch terminal, as upstream's `surfaceEnv`).
    pub token: String,
    pub cwd: String,
    /// What the pane runs instead of the login shell.
    pub command: Option<String>,
    /// Hold the pane after the command exits.
    pub wait: bool,
}

impl PaneSpec {
    /// The primary pane of `session` in `store`, `None` when the session is not in its tree.
    #[must_use]
    pub fn primary(window: Uuid, store: &AppStore, session: Uuid) -> Option<Self> {
        let workspace = store.workspace_for_session(session)?.id();
        let session = store.session(session)?;
        Some(Self {
            window,
            workspace,
            session: session.id(),
            pane: StatusPane::Left,
            token: pane_env::uuid_string(session.pane_identity),
            cwd: session.effective_cwd().to_owned(),
            command: session.launch.command.clone(),
            wait: session.launch.wait,
        })
    }

    /// The split pane of `session`: its own cwd and creation command; `None` without a split identity.
    #[must_use]
    pub fn split(window: Uuid, workspace: Uuid, session: &Session) -> Option<Self> {
        Some(Self {
            window,
            workspace,
            session: session.id(),
            pane: StatusPane::Right,
            token: pane_env::uuid_string(session.split_pane_identity?),
            cwd: session.cwd_for(StatusPane::Right).to_owned(),
            command: session.split_launch.command.clone(),
            wait: session.split_launch.wait,
        })
    }

    /// A scratch terminal of `session`, in the primary's cwd, running the pending scratch command once (it is taken)
    /// or a login shell.
    #[must_use]
    pub fn scratch(window: Uuid, workspace: Uuid, session: &mut Session) -> Self {
        Self {
            window,
            workspace,
            session: session.id(),
            pane: StatusPane::Scratch,
            token: pane_env::uuid_string(Uuid::new_v4()),
            cwd: session.effective_cwd().to_owned(),
            command: session.scratch_command.take(),
            wait: false,
        }
    }

    /// The pane token baked into the shell and reported by the session's surface slot.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The pane's environment: the session, window and workspace ids, `AGTERM_PANE` and the pane token.
    #[must_use]
    pub fn env(&self, context: &PaneContext) -> PaneEnv {
        pane_env::session(&SessionPane {
            session: self.session,
            window: Some(self.window),
            workspace: Some(self.workspace),
            socket_path: &context.socket,
            program_version: &context.program_version,
            pane: Some(self.pane),
            pane_token: Some(&self.token),
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
    /// The primary panes of sessions without a page yet, in tree order.
    pub add: Vec<PaneSpec>,
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
        .filter_map(|session| PaneSpec::primary(window, store, session.id()))
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

/// What every page's reconcile shares.
struct PageSync<'a> {
    window: Uuid,
    zoom: Option<SurfaceId>,
    /// The window's open dashboard.
    grid: Option<&'a GridModel>,
    context: &'a PaneContext,
    poster: &'a Poster,
}

/// One session's page: its panes and their container.
struct Page {
    panes: PaneView,
}

impl Page {
    /// Brings the page in line with its session: the terminals the model dropped go, the panes it lays out and has
    /// no terminal for are created and installed in their slots, and the layout follows the session and the zoom.
    fn sync(&mut self, core: &mut Core, sync: &PageSync<'_>, id: Uuid, shown: bool) {
        let Some(store) = core.library_mut().store_mut(sync.window) else {
            self.panes.hide();
            return;
        };
        let workspace = store.workspace_for_session(id).map(Workspace::id);
        let (Some(workspace), Some(session)) = (workspace, store.session_mut(id)) else {
            // held by a pending soft close: its terminals keep running, hidden, until the close is final or undone
            self.panes.hide();
            return;
        };
        self.panes.retain(|token| session.pane_role_for_token(token).is_some());
        let grid = sync.grid.filter(|grid| grid.hosts(id));
        let needs = split::needs(session, sync.zoom);
        if (needs.split || grid.is_some_and(|grid| grid_needs_split(grid, session)))
            && let Some(spec) = PaneSpec::split(sync.window, workspace, session)
        {
            self.mount(session, &spec, sync);
        }
        if needs.scratch {
            let spec = PaneSpec::scratch(sync.window, workspace, session);
            self.mount(session, &spec, sync);
        }
        // the first layout of a shown split seeds an even ratio, so a shown split always reports one (upstream's
        // `restoreDivider`); a zoomed window lays no split out
        if sync.zoom.is_none() && session.split.shown && session.split.ratio.is_none() {
            session.split.ratio = Some(SPLIT_RATIO_DEFAULT);
        }
        match grid {
            Some(grid) => self.panes.apply_grid(&grid_cells(grid, session), grid.members.len()),
            None => self.panes.apply(PanePlan::layout(session, sync.zoom), shown),
        }
        crate::background::sync_page(&self.panes, session);
    }

    /// Creates the terminal of `spec`, mounts it and installs it in its slot of `session`.
    fn mount(&mut self, session: &mut Session, spec: &PaneSpec, sync: &PageSync<'_>) {
        let terminal = spawn(spec, sync.context, sync.poster);
        self.panes
            .add(spec.token.clone(), terminal.clone(), spec.pane == StatusPane::Scratch);
        install(session, spec, terminal);
    }
}

/// Whether the grid hosts `session`'s split pane while it has no terminal yet: a hidden split's unfocused pane is
/// created for its cell, as upstream's cell mounts the pane it hosts.
fn grid_needs_split(grid: &GridModel, session: &Session) -> bool {
    grid.cells_of(session.id()).any(|(_, pane)| pane == ZoomSurface::Split)
        && session.split.exists
        && session.split_pane_identity.is_some()
        && !session.split_surface.is_present()
}

/// The pane token and cell index of each of `session`'s grid cells that has a terminal.
fn grid_cells(grid: &GridModel, session: &Session) -> Vec<(String, usize)> {
    grid.cells_of(session.id())
        .filter_map(|(index, surface)| {
            let surface = session.pane_slot(surface.pane()?).get()?;
            Some((surface.pane_token().to_owned(), index))
        })
        .collect()
}

/// A connected terminal for `spec`.
fn spawn(spec: &PaneSpec, context: &PaneContext, poster: &Poster) -> TerminalSurface {
    let terminal = TerminalSurface::new(spec.surface_spec(context));
    connect_terminal(&terminal, spec.session, spec.token(), poster);
    connect_reports(&terminal, spec.session, spec.token(), poster);
    terminal
}

/// Puts `terminal` in the slot of `spec`'s pane; a terminal already there is torn down (it never should be).
fn install(session: &mut Session, spec: &PaneSpec, terminal: TerminalSurface) {
    let surface = Box::new(PaneSurface::new(terminal, spec.token.clone()));
    if let Some(previous) = session.pane_slot_mut(spec.pane).replace(surface) {
        tracing::error!(session = %spec.session, pane = spec.pane.as_str(), "a pane got a second terminal");
        previous.teardown();
    }
}

/// The deck widget: the "No session selected" placeholder under one overlay page per session, all under the
/// dashboard's chrome.
pub struct Deck {
    /// The pages' overlay under the chrome.
    root: gtk::Overlay,
    overlay: gtk::Overlay,
    placeholder: gtk::Label,
    pages: HashMap<Uuid, Page>,
    shown: Option<Uuid>,
    /// The page and pane token keyboard focus was last moved to.
    focused: Option<(Uuid, String)>,
    chrome: Chrome,
    /// The window's dashboard was open at the last reconcile.
    in_grid: bool,
    fonts: FontOverrides,
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
        let chrome = Chrome::new();
        let root = gtk::Overlay::builder().hexpand(true).vexpand(true).build();
        root.set_child(Some(&overlay));
        root.add_overlay(chrome.widget());
        Self {
            root,
            overlay,
            placeholder,
            pages: HashMap::new(),
            shown: None,
            focused: None,
            chrome,
            in_grid: false,
            fonts: FontOverrides::default(),
        }
    }

    /// The widget to place in the window.
    #[must_use]
    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    /// The dashboard's chrome, whose clicks and keys the window connects.
    #[must_use]
    pub fn chrome(&self) -> &Chrome {
        &self.chrome
    }

    /// The shown page's focused terminal; none while the dashboard is open.
    #[must_use]
    pub fn shown_terminal(&self) -> Option<&TerminalSurface> {
        self.shown
            .and_then(|id| self.pages.get(&id))
            .and_then(|page| page.panes.focus())
            .map(|(_, terminal)| terminal)
    }

    /// Gives the keyboard to what should hold it: the dashboard's chrome while it is open, else the shown terminal.
    pub fn grab_focus(&self) {
        if self.in_grid {
            self.chrome.grab_focus();
        } else if let Some(terminal) = self.shown_terminal() {
            terminal.grab_focus();
        }
    }

    /// Brings the deck in line with `window`'s sessions: pages for new sessions, gone sessions' pages removed, every
    /// page's panes synced with its session, the selected session's page shown and its focused pane given the
    /// keyboard when that changed. While the window is zoomed into one of its terminals, that session's page shows
    /// instead, selected or not, with that terminal alone (upstream's zoom layer; a terminal is never reparented, so
    /// its own page fills). While the window's dashboard is open its member pages show instead, laid out as the grid.
    pub fn reconcile(
        &mut self,
        core: &mut Core,
        window: Option<Uuid>,
        zoom: Option<SurfaceId>,
        context: &PaneContext,
        poster: &Poster,
    ) {
        let mounted: HashSet<Uuid> = self.pages.keys().copied().collect();
        let plan = plan(core.library(), window, &mounted);
        for id in &plan.remove {
            if let Some(page) = self.pages.remove(id) {
                // the session's close tore the terminals down already; this only drops the widgets
                page.panes.teardown();
                self.overlay.remove_overlay(page.panes.widget());
            }
        }
        for spec in &plan.add {
            self.add_page(core, spec, context, poster);
        }
        let zoom = zoom.filter(|zoom| self.pages.contains_key(&zoom.session));
        let shown = zoom.map(|zoom| zoom.session).or(plan.shown);
        let grid = window.and_then(|window| GridModel::of(core, window));
        if let Some(window) = window {
            let sync = PageSync {
                window,
                zoom,
                grid: grid.as_ref(),
                context,
                poster,
            };
            for (id, page) in &mut self.pages {
                // under the grid only its member pages show
                page.sync(core, &sync, *id, grid.is_none() && Some(*id) == shown);
            }
        }
        if let Some(grid) = &grid {
            let store = window.and_then(|window| core.library().store(window));
            self.show_grid(store, grid);
            return;
        }
        if self.in_grid {
            self.leave_grid();
        }
        // a zoom keeps the keyboard on its terminal (upstream's `focusSplitPane` steps aside while zoomed)
        let refocus = zoom.is_none() && shown.is_some_and(|id| core.pane_focus_requested(id));
        self.show(shown, refocus);
    }

    /// Shows the dashboard: every member page at once over the backdrop (a page shows, but takes no input: the chrome
    /// does), the chrome with the keyboard, and the member terminals at the grid's font size.
    fn show_grid(&mut self, store: Option<&AppStore>, grid: &GridModel) {
        for (id, page) in &self.pages {
            let widget = page.panes.widget();
            if grid.hosts(*id) {
                widget.set_opacity(1.0);
                widget.set_can_target(false);
                widget.set_can_focus(false);
            } else if widget.opacity() > 0.0 || widget.can_target() {
                set_page_visible(widget, false);
            }
        }
        self.placeholder.set_visible(false);
        self.overlay.add_css_class(dashboard::BACKDROP_CLASS);
        self.chrome.update(&grid.chrome_cells(store));
        self.chrome.grab_focus();
        let terminals = store
            .map(|store| self.member_terminals(store, grid))
            .unwrap_or_default();
        self.fonts.sync(&terminals, grid.font_size);
        self.in_grid = true;
        self.shown = None;
        self.focused = None;
    }

    /// The dashboard closed: the backdrop and chrome go, the member terminals get their own font back, and the next
    /// show gives the keyboard to the shown page's focused pane again.
    fn leave_grid(&mut self) {
        self.overlay.remove_css_class(dashboard::BACKDROP_CLASS);
        self.chrome.update(&[]);
        self.fonts.sync(&[], None);
        self.in_grid = false;
        self.focused = None;
    }

    /// The terminals of the grid's cells, in cell order.
    fn member_terminals(&self, store: &AppStore, grid: &GridModel) -> Vec<TerminalSurface> {
        grid.members
            .iter()
            .filter_map(|member| {
                let session = store.session(member.session)?;
                let token = session.pane_slot(member.surface.pane()?).get()?.pane_token().to_owned();
                self.pages.get(&member.session)?.panes.terminal(&token).cloned()
            })
            .collect()
    }

    fn add_page(&mut self, core: &mut Core, spec: &PaneSpec, context: &PaneContext, poster: &Poster) {
        let mut panes = PaneView::new(spec.session, poster);
        set_page_visible(panes.widget(), false);
        self.overlay.add_overlay(panes.widget());
        let terminal = spawn(spec, context, poster);
        panes.add(spec.token.clone(), terminal.clone(), false);
        let session = core
            .library_mut()
            .store_mut(spec.window)
            .and_then(|store| store.session_mut(spec.session));
        if let Some(session) = session {
            install(session, spec, terminal);
        }
        self.pages.insert(spec.session, Page { panes });
    }

    /// Shows the page of `shown` alone and gives its focused pane the keyboard when the page or that pane changed
    /// (upstream focuses the active session after a select or a create, and the focused pane within it), or when
    /// `refocus` (a `session.focus` asked for it, wherever focus sits now); with none shown the focus leaves the
    /// hidden terminals.
    fn show(&mut self, shown: Option<Uuid>, refocus: bool) {
        for (id, page) in &self.pages {
            let visible = Some(*id) == shown;
            let widget = page.panes.widget();
            // a grid page shows without taking input: both switches are checked
            if widget.can_target() != visible || (widget.opacity() > 0.0) != visible {
                set_page_visible(widget, visible);
            }
        }
        self.placeholder.set_visible(shown.is_none());
        let changed = self.shown != shown;
        self.shown = shown;
        let Some(id) = shown else {
            self.focused = None;
            if changed && let Some(root) = self.overlay.root() {
                root.set_focus(None::<&gtk::Widget>);
            }
            return;
        };
        let focus = self.pages.get(&id).and_then(|page| page.panes.focus());
        let desired = focus.map(|(token, _)| (id, token.to_owned()));
        if desired == self.focused && !refocus {
            return;
        }
        if focus.is_some_and(|(_, terminal)| terminal.grab_focus()) || desired.is_none() {
            self.focused = desired;
        }
    }
}

impl Default for Deck {
    fn default() -> Self {
        Self::new()
    }
}

/// The page-level visibility switch (see the module docs): a hidden page stays mapped and allocated so its surfaces
/// exist, but is not drawn, hit-tested or focusable. Its terminals' own switch is the pane layout's.
fn set_page_visible(widget: &split::PaneStack, visible: bool) {
    widget.set_opacity(if visible { 1.0 } else { 0.0 });
    widget.set_can_target(visible);
    widget.set_can_focus(visible);
}

/// Routes the terminal's lifecycle and input to the model as main-loop tasks, by the pane token it carries: its shell
/// exiting closes its pane by the pane's role now (upstream's per-pane `onExit`), the user focusing it moves the
/// split's focus there (unless the focus moved on before the task ran, or the pane is hidden), and a key press clears
/// the session's status when this pane owns it (upstream's `wireStatusClear`).
fn connect_terminal(terminal: &TerminalSurface, session: Uuid, token: &str, poster: &Poster) {
    let posting = poster.clone();
    let exit_token = token.to_owned();
    terminal.connect_exit(move || {
        state::post(&posting, move |state: &mut AppState| {
            state.pane_exited(session, &exit_token);
        });
    });
    let posting = poster.clone();
    let focus_token = token.to_owned();
    let key = terminal.key();
    terminal.connect_focus_in(move || {
        let token = focus_token.clone();
        state::post(&posting, move |state: &mut AppState| {
            let current = surface::lookup(key).is_some_and(|terminal| still_focused(&terminal));
            if current {
                state.pane_focused(session, &token);
            }
        });
    });
    let posting = poster.clone();
    let keystroke_token = token.to_owned();
    terminal.connect_keystroke(move |keystroke| {
        let token = keystroke_token.clone();
        state::post(&posting, move |state: &mut AppState| {
            state.core.note_user_activity(session);
            state.arm_auto_follow();
            if state.core.clear_status_on_keystroke(session, &token, keystroke) {
                state.settle();
            }
        });
    });
}

/// Whether a terminal that reported taking focus still has it and is on screen: focus GTK moves while panes come and
/// go must not undo the pane the model chose.
fn still_focused(terminal: &TerminalSurface) -> bool {
    terminal.widget().has_focus() && terminal.is_deck_visible()
}

/// Routes the terminal's reports to the model as main-loop tasks: its search reports on the session's search
/// (upstream's `wireSearchCallbacks`), its cwd and title reports on its pane's fields by role, and a clicked link
/// through the link policy as a click in this session's pane, under the link-open setting.
fn connect_reports(terminal: &TerminalSurface, session: Uuid, token: &str, poster: &Poster) {
    let posting = poster.clone();
    let search_token = token.to_owned();
    terminal.connect_search(move |report| {
        let (report, token) = (report.clone(), search_token.clone());
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
    let pwd_token = token.to_owned();
    terminal.connect_pwd(move |pwd| {
        let (report, token) = (PaneReport::Pwd(pwd.to_owned()), pwd_token.clone());
        state::post(&posting, move |state: &mut AppState| {
            state.report_pane(session, &token, &report);
        });
    });
    let posting = poster.clone();
    let title_token = token.to_owned();
    terminal.connect_title(move |title| {
        let (report, token) = (PaneReport::Title(title.to_owned()), title_token.clone());
        state::post(&posting, move |state: &mut AppState| {
            state.report_pane(session, &token, &report);
        });
    });
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{NewSession, SplitAxis};
    use agterm_proto::command::SessionCmd;
    use agterm_proto::{Args, Command, Request};

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
            let spec = PaneSpec::primary(window, store, id).unwrap();
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
            PaneSpec::primary(window, library.store(window).unwrap(), Uuid::new_v4()),
            None
        );
    }

    #[test]
    fn the_split_pane_runs_in_its_own_cwd_with_its_identity_and_command() {
        let (mut library, window, ids) = three_sessions();
        let store = library.store_mut(window).unwrap();
        let workspace = store.workspaces()[0].id();
        assert_eq!(
            PaneSpec::split(window, workspace, store.session(ids[0]).unwrap()),
            None,
            "no split yet"
        );
        store.set_split_visibility(ids[0], true, None);
        let session = store.session_mut(ids[0]).unwrap();
        session.split_cwd = Some("/srv/right".to_owned());
        session.split_launch.command = Some("htop".to_owned());
        session.split_launch.wait = true;
        let session = store.session(ids[0]).unwrap();
        let spec = PaneSpec::split(window, workspace, session).unwrap();
        let env = spec.env(&context());
        assert_eq!(env["AGTERM_PANE"], "right");
        let identity = pane_env::uuid_string(session.split_pane_identity.unwrap());
        assert_eq!(env["AGTERM_PANE_ID"], identity);
        assert_eq!(spec.token(), identity);
        assert_eq!(env["AGTERM_SESSION_ID"], pane_env::uuid_string(ids[0]));
        let surface = spec.surface_spec(&context());
        assert_eq!(surface.working_directory, Some(PathBuf::from("/srv/right")));
        assert_eq!(session.cwd_for(StatusPane::Right), "/srv/right");
        assert_eq!(surface.command.as_deref(), Some("htop"));
        assert!(surface.wait_after_command);
    }

    #[test]
    fn each_scratch_terminal_gets_a_fresh_token_and_runs_its_command_once() {
        let (mut library, window, ids) = three_sessions();
        let store = library.store_mut(window).unwrap();
        let workspace = store.workspaces()[0].id();
        let session = store.session_mut(ids[1]).unwrap();
        session.scratch_command = Some("lazygit".to_owned());
        let first = PaneSpec::scratch(window, workspace, session);
        assert_eq!(first.command.as_deref(), Some("lazygit"));
        assert_eq!(session.scratch_command, None, "taken: it runs once");
        let env = first.env(&context());
        assert_eq!(env["AGTERM_PANE"], "scratch");
        assert_eq!(env["AGTERM_PANE_ID"], first.token());
        assert_eq!(first.cwd, "/srv");
        assert!(!first.wait);
        let second = PaneSpec::scratch(window, workspace, session);
        assert_eq!(second.command, None, "a respawn is a login shell");
        assert_ne!(second.token(), first.token());
        assert_ne!(first.token(), pane_env::uuid_string(session.pane_identity));
    }

    fn test_core(library: WindowLibrary) -> Core {
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

    #[test]
    fn pane_terminals_never_change_parent() {
        crate::gtk_test::on_gtk_thread(pane_terminals_keep_their_parent);
    }

    #[test]
    fn session_focus_gives_an_unchanged_pane_the_keyboard_back() {
        crate::gtk_test::on_gtk_thread(focus_request_regrabs);
    }

    /// `session.focus` naming the pane the model already focused, after focus left the terminals (a sidebar click),
    /// gives that pane the keyboard again (upstream's `setSplitFocus` grabs whatever the model held); an ordinary
    /// render does not pull focus back.
    fn focus_request_regrabs() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_child(Some(walk.deck.widget()));
        walk.settle();
        let split = walk.probe().area(&walk.split);
        assert!(split.is_focus(), "a new split's right pane has the keyboard");

        GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        walk.settle();
        assert!(!split.is_focus(), "a render leaves focus where the user put it");

        let request = Request {
            cmd: Command::Session(SessionCmd::Focus),
            target: Some(agterm_core::pane_env::uuid_string(walk.session)),
            args: Some(Args {
                pane: Some("right".to_owned()),
                ..Args::default()
            }),
        };
        assert!(walk.core.dispatch(&request).ok);
        walk.settle();
        assert!(split.is_focus(), "the requested pane took the keyboard back");
        walk.core.clear_pane_focus_requests();

        GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        walk.settle();
        assert!(!split.is_focus(), "the request was one-shot");
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    #[test]
    fn the_dashboard_lays_member_panes_out_in_place() {
        crate::gtk_test::on_gtk_thread(grid_lays_panes_out_in_place);
    }

    /// The dashboard over a split session shows both of its terminals at their cells, view-only, without moving them;
    /// closing it gives them their page and their input back.
    fn grid_lays_panes_out_in_place() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_child(Some(walk.deck.widget()));
        walk.settle();
        let request = Request {
            cmd: Command::Dashboard,
            target: None,
            args: Some(Args {
                targets: Some(vec![agterm_core::pane_env::uuid_string(walk.session)]),
                font_size: Some(9.0),
                ..Args::default()
            }),
        };
        assert!(walk.core.dispatch(&request).ok);
        walk.settle();
        assert!(walk.deck.in_grid);
        assert!(walk.deck.chrome().widget().get_visible());
        for token in [&walk.primary, &walk.split] {
            let terminal = walk.probe().terminal(token).clone();
            assert!(terminal.is_deck_visible(), "{token} shows in its cell");
            assert!(
                !terminal.widget().is_focusable() && !terminal.widget().can_target(),
                "view-only"
            );
        }
        walk.stack.size_allocate(&gtk::Allocation::new(0, 0, 812, 612), -1);
        let (primary, split) = &walk.areas;
        assert_eq!((primary.width(), primary.height()), (388, 588));
        assert_eq!((split.width(), split.height()), (388, 588));
        assert!(walk.deck.shown_terminal().is_none(), "the chrome holds the keyboard");

        walk.core.dashboard_close(walk.window);
        walk.settle();
        assert!(!walk.deck.in_grid);
        assert!(!walk.deck.chrome().widget().get_visible());
        assert_eq!(walk.shown(), (true, true));
        assert!(walk.probe().terminal(&walk.primary).widget().is_focusable());
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    /// A session's page in the deck, read by pane token.
    struct Probe<'a> {
        deck: &'a Deck,
        session: Uuid,
    }

    impl Probe<'_> {
        fn terminal(&self, token: &str) -> &TerminalSurface {
            self.deck.pages[&self.session].panes.terminal(token).unwrap()
        }

        fn area(&self, token: &str) -> gtk::GLArea {
            self.terminal(token).widget().clone()
        }

        fn visible(&self, token: &str) -> bool {
            self.terminal(token).is_deck_visible()
        }

        fn focus(&self) -> Option<String> {
            self.deck.pages[&self.session]
                .panes
                .focus()
                .map(|(token, _)| token.to_owned())
        }
    }

    fn slot_token(core: &Core, window: Uuid, session: Uuid, pane: StatusPane) -> String {
        let session = core.library().store(window).unwrap().session(session).unwrap();
        session.pane_slot(pane).get().unwrap().pane_token().to_owned()
    }

    fn store(core: &mut Core, window: Uuid) -> &mut AppStore {
        core.library_mut().store_mut(window).unwrap()
    }

    /// One page's walk through every layout change: the core, the deck, and the two pane terminals it started with.
    struct Walk {
        core: Core,
        deck: Deck,
        window: Uuid,
        session: Uuid,
        primary: String,
        split: String,
        stack: gtk::Widget,
        areas: (gtk::GLArea, gtk::GLArea),
    }

    impl Walk {
        /// A seeded window whose selected session was just split.
        fn split() -> Self {
            let library = WindowLibrary::seeded("/home/u");
            let window = library.active_window_id().unwrap();
            let session = library.store(window).unwrap().selected_session_id().unwrap();
            let mut core = test_core(library);
            let mut deck = Deck::new();
            reconcile(&mut deck, &mut core, window);
            let primary = slot_token(&core, window, session, StatusPane::Left);
            store(&mut core, window).set_split_visibility(session, true, None);
            reconcile(&mut deck, &mut core, window);
            let split = slot_token(&core, window, session, StatusPane::Right);
            let stack = deck.pages[&session].panes.widget().clone().upcast();
            let probe = Probe { deck: &deck, session };
            let areas = (probe.area(&primary), probe.area(&split));
            Self {
                core,
                deck,
                window,
                session,
                primary,
                split,
                stack,
                areas,
            }
        }

        fn store(&mut self) -> &mut AppStore {
            store(&mut self.core, self.window)
        }

        fn probe(&self) -> Probe<'_> {
            Probe {
                deck: &self.deck,
                session: self.session,
            }
        }

        /// Reconciles, then checks both pane terminals are the same widgets in the same container.
        fn settle(&mut self) {
            reconcile(&mut self.deck, &mut self.core, self.window);
            let probe = self.probe();
            for token in [&self.primary, &self.split] {
                assert_eq!(probe.area(token).parent().as_ref(), Some(&self.stack), "{token}");
            }
            assert_eq!(probe.area(&self.primary), self.areas.0);
            assert_eq!(probe.area(&self.split), self.areas.1);
        }

        /// Whether the original primary and split terminals are visible.
        fn shown(&self) -> (bool, bool) {
            let probe = self.probe();
            (probe.visible(&self.primary), probe.visible(&self.split))
        }
    }

    fn reconcile(deck: &mut Deck, core: &mut Core, window: Uuid) {
        let zoom = core.zoom_target(window);
        deck.reconcile(core, Some(window), zoom, &context(), &state::detached_poster());
    }

    fn pane_terminals_keep_their_parent() {
        let mut walk = Walk::split();
        assert_eq!(walk.shown(), (true, true));
        assert_eq!(
            walk.probe().focus(),
            Some(walk.split.clone()),
            "a new split focuses right"
        );
        let session = walk.session;
        let ratio = walk.store().session(session).unwrap().split.ratio;
        assert_eq!(
            ratio,
            Some(SPLIT_RATIO_DEFAULT),
            "a shown split's first layout seeds its ratio"
        );

        walk.store().set_split_visibility(session, false, None);
        walk.settle();
        assert_eq!(walk.shown(), (false, true), "the hidden split shows right maximized");

        walk.store().swap_panes(session).unwrap();
        walk.settle();
        assert_eq!(
            slot_token(&walk.core, walk.window, session, StatusPane::Left),
            walk.split
        );
        assert_eq!(walk.shown(), (false, true), "focus followed the terminal");
        assert_eq!(walk.probe().focus(), Some(walk.split.clone()));

        walk.store()
            .set_split_visibility(session, true, Some(SplitAxis::TopBottom));
        walk.settle();
        assert_eq!(walk.shown(), (true, true));

        let zoomed = SurfaceId {
            session,
            surface: agterm_core::zoom::ZoomSurface::Split,
        };
        walk.core.set_zoom_target(walk.window, Some(zoomed));
        walk.settle();
        assert_eq!(walk.shown(), (true, false), "the split slot holds the old primary now");
        assert_eq!(walk.probe().focus(), Some(walk.primary.clone()));
        walk.core.set_zoom_target(walk.window, None);
        scratch_covers_and_closes(walk);
    }

    fn scratch_covers_and_closes(mut walk: Walk) {
        let session = walk.session;
        walk.store().toggle_scratch(session, true);
        walk.settle();
        let scratch = slot_token(&walk.core, walk.window, session, StatusPane::Scratch);
        assert!(walk.probe().visible(&scratch), "the scratch covers");
        assert_eq!(walk.shown(), (false, false));
        assert_eq!(walk.probe().focus(), Some(scratch.clone()));
        assert_eq!(walk.probe().area(&scratch).parent().as_ref(), Some(&walk.stack));

        walk.store().toggle_scratch(session, true);
        walk.settle();
        assert!(!walk.probe().visible(&scratch));
        assert_eq!(walk.shown(), (true, true));
        walk.stack.size_allocate(&gtk::Allocation::new(0, 0, 800, 601), -1);
        let (bottom, top) = &walk.areas;
        assert_eq!(
            (top.width(), top.height()),
            (800, 300),
            "the swapped-in terminal sits on top"
        );
        assert_eq!((bottom.width(), bottom.height()), (800, 300));
        assert_eq!(
            walk.probe().area(&scratch).height(),
            601,
            "the hidden scratch keeps the full page"
        );

        assert!(walk.store().close_scratch(session, true));
        walk.store().close_split(session);
        reconcile(&mut walk.deck, &mut walk.core, walk.window);
        let page = &walk.deck.pages[&session].panes;
        assert!(page.terminal(&scratch).is_none() && page.terminal(&walk.primary).is_none());
        assert_eq!(walk.areas.1.parent().as_ref(), Some(&walk.stack), "the survivor stays");
        assert!(walk.areas.0.parent().is_none(), "the closed split's widget is gone");
    }
}
