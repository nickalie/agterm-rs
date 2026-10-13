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
//! A terminal ask on the shown page takes the keyboard in place of the terminal it covers while it may
//! ([`crate::ask_dialog`]); while the window's picker or GUI ask is up no terminal is given the keyboard, and the shown
//! terminal gets it back once the slot frees.
//!
//! The plan of what to add, remove and show is a pure function of the model ([`plan`]), so it is testable without a
//! display.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agterm_core::keymap::SurfaceOwner;
use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::pane::SPLIT_RATIO_DEFAULT;
use agterm_core::model::{AppStore, Session, WindowLibrary, Workspace};
use agterm_core::pane_env::{self, PaneEnv, SessionPane, StatusPane};
use agterm_core::zoom::{SurfaceId, ZoomSurface};
use agterm_core::{Core, PaneReplacement, PaneReport};
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
#[derive(Debug, Clone, PartialEq)]
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
    /// The point size the terminal starts at: the session's persisted size, `None` for the configured one.
    pub font_size: Option<f64>,
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
            font_size: session.font_size,
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
            font_size: session.font_size,
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
            font_size: session.font_size,
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
        #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
        let font_size = self.font_size.map(|size| size as f32);
        SurfaceSpec {
            working_directory: Some(PathBuf::from(&self.cwd)),
            command: self.command.clone(),
            env: self.env(context).into_iter().collect(),
            wait_after_command: self.wait,
            font_size,
            live: false,
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
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// Something other than the window's terminals holds the keyboard: its picker or GUI ask, or the quick terminal.
    focus_blocked: bool,
    look: &'a crate::ask_dialog::Look,
}

/// One session's page: its panes and their container.
struct Page {
    panes: PaneView,
}

impl Page {
    /// Brings the page in line with its session: the terminals the model dropped go, the panes it lays out and has
    /// no terminal for are created and installed in their slots, and the layout follows the session and the zoom.
    fn sync(&mut self, core: &mut Core, sync: &PageSync<'_>, id: Uuid, shown: bool) {
        let covers = lead_covers(core, sync.window, id);
        let Some(store) = core.library_mut().store_mut(sync.window) else {
            self.panes.hide();
            return;
        };
        let workspace = store.workspace_for_session(id).map(Workspace::id);
        if workspace.is_none() || store.session(id).is_none() {
            // held by a pending soft close: its terminals keep running, hidden, until the close is final or undone; its
            // pages stay but are off screen, so they ask nothing and a prompt they had goes
            self.panes.hide();
            if let Some(session) = store.pending_close_session(id) {
                crate::overlay::hide_pages(session);
            }
            return;
        }
        let (Some(workspace), Some(session)) = (workspace, store.session_mut(id)) else {
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
        let plan = if let Some(grid) = grid {
            self.panes.apply_grid(&grid_cells(grid, session), grid.members.len());
            None
        } else {
            let plan = PanePlan::layout(session, sync.zoom);
            self.panes.apply(plan.clone(), shown);
            Some(plan)
        };
        self.panes.sync_lead_covers(&covers, shown);
        let hud = crate::hud::HudContext {
            window: sync.window,
            workspace,
            context: sync.context,
            poster: sync.poster,
        };
        crate::overlay::sync_page(&mut self.panes, session, &hud, plan.as_ref(), shown);
        crate::hud::sync_page(&mut self.panes, session, &hud, plan.as_ref(), shown);
        let ask = crate::ask_dialog::AskPage {
            shown: shown && sync.zoom.is_none() && sync.grid.is_none(),
            focus_blocked: sync.focus_blocked,
            look: sync.look,
            poster: sync.poster,
        };
        crate::ask_dialog::sync_page(&mut self.panes, session, &ask);
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

/// The lead cover each of `session`'s panes needs now (see [`crate::pane_lead`]).
fn lead_covers(core: &Core, window: Uuid, session: Uuid) -> Vec<(String, crate::pane_lead::PaneCover)> {
    let tokens: Vec<String> = core
        .library()
        .store(window)
        .and_then(|store| store.session(session))
        .map(|session| {
            [StatusPane::Left, StatusPane::Right]
                .into_iter()
                .filter_map(|pane| session.pane_slot(pane).get())
                .map(|surface| surface.pane_token().to_owned())
                .collect()
        })
        .unwrap_or_default();
    crate::pane_lead::covers(core, &tokens)
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
    terminal.set_owner(SurfaceOwner::Pane {
        session: spec.session,
        token: spec.token().to_owned(),
    });
    connect_terminal(&terminal, spec.session, spec.token(), poster);
    connect_reports(&terminal, spec.session, spec.token(), poster);
    crate::pane_lead::connect_takeover(&terminal, spec.session, spec.token(), poster);
    if spec.pane != StatusPane::Scratch {
        // a session pane spawns with the seed the core decides when it is ready, after its paced launch turn
        let (session, token, poster) = (spec.session, spec.token().to_owned(), poster.clone());
        terminal.gate_launch(move || {
            let token = token.clone();
            state::post(&poster, move |state: &mut AppState| {
                state.pane_spawn_ready(session, &token);
            });
        });
    }
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
    /// The panes this deck gave focus to since the last [`Deck::take_given_focus`], reported as focus moves even when
    /// the terminal held the keyboard already and GTK sends no focus-enter.
    given_focus: Vec<(Uuid, String)>,
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
            given_focus: Vec::new(),
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

    /// Gives the keyboard to what should hold it: the dashboard's chrome while it is open, else the shown page's
    /// terminal ask while it takes the keyboard, else the HTML page covering the shown session, else the shown
    /// terminal.
    pub fn grab_focus(&self) {
        if self.in_grid {
            self.chrome.grab_focus();
        } else if let Some((_, dialog)) = self.shown_ask_focus() {
            dialog.grab_focus();
        } else if let Some((_, page)) = self.shown_page_focus() {
            page.grab_focus();
        } else if let Some(terminal) = self.shown_terminal() {
            terminal.grab_focus();
        }
    }

    /// The shown page's terminal ask, while it takes the keyboard in place of its terminal.
    fn shown_ask_focus(&self) -> Option<(String, gtk::Widget)> {
        self.shown
            .and_then(|id| self.pages.get(&id))
            .and_then(|page| page.panes.ask().focus_target())
    }

    /// The HTML page covering the shown session, which takes the keyboard in place of its terminal.
    fn shown_page_focus(&self) -> Option<(String, gtk::Widget)> {
        self.shown
            .and_then(|id| self.pages.get(&id))
            .and_then(|page| page.panes.page_focus())
    }

    /// The page of session `session`, which a GUI ask anchored to it is drawn over.
    #[must_use]
    pub fn page_stack(&self, session: Uuid) -> Option<split::PaneStack> {
        self.pages.get(&session).map(|page| page.panes.widget().clone())
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
        let modal = window.is_some_and(|window| core.modal_pending(window));
        if let Some(window) = window {
            let look = crate::ask_dialog::Look::current(core);
            let sync = PageSync {
                window,
                zoom,
                grid: grid.as_ref(),
                context,
                poster,
                focus_blocked: modal || core.quick_visible(),
                look: &look,
            };
            for (id, page) in &mut self.pages {
                // under the grid only its member pages show
                page.sync(core, &sync, *id, grid.is_none() && Some(*id) == shown);
            }
        }
        if let Some(grid) = &grid {
            let store = window.and_then(|window| core.library().store(window));
            self.show_grid(store, grid, modal);
            return;
        }
        if self.in_grid {
            self.leave_grid();
        }
        // a zoom keeps the keyboard on its terminal (upstream's `focusSplitPane` steps aside while zoomed)
        let refocus = zoom.is_none() && shown.is_some_and(|id| core.pane_focus_requested(id));
        self.show(shown, refocus, modal);
    }

    /// Shows the dashboard: every member page at once over the backdrop (a page shows, but takes no input: the chrome
    /// does), the chrome with the keyboard unless the window's picker or GUI ask holds it (`modal`), and the member
    /// terminals at the grid's font size.
    fn show_grid(&mut self, store: Option<&AppStore>, grid: &GridModel, modal: bool) {
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
        if !modal {
            self.chrome.grab_focus();
        }
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
    /// hidden terminals. While the window's picker or GUI ask is up (`modal`) nothing is focused or unfocused (the
    /// modal holds the keyboard, also when the window's last session goes), and the next show after it focuses again.
    fn show(&mut self, shown: Option<Uuid>, refocus: bool, modal: bool) {
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
            if changed
                && !modal
                && let Some(root) = self.overlay.root()
            {
                root.set_focus(None::<&gtk::Widget>);
            }
            return;
        };
        if modal {
            self.focused = None;
            return;
        }
        self.focus_page(id, refocus);
    }

    /// Gives page `id`'s terminal ask or focused pane the keyboard when that changed or `refocus` asks for it.
    fn focus_page(&mut self, id: Uuid, refocus: bool) {
        if let Some((key, dialog)) = self.shown_ask_focus() {
            let desired = Some((id, key));
            if (desired != self.focused || refocus) && dialog.grab_focus() {
                self.focused = desired;
            }
            return;
        }
        // a page reports its own focus to the model (its web view's focus-enter), so it is not a given pane focus
        if let Some((key, page)) = self.shown_page_focus() {
            let desired = Some((id, key));
            if (desired != self.focused || refocus) && page.grab_focus() {
                self.focused = desired;
            }
            return;
        }
        let focus = self.pages.get(&id).and_then(|page| page.panes.focus());
        let desired = focus.map(|(token, _)| (id, token.to_owned()));
        if desired == self.focused && !refocus {
            return;
        }
        if focus.is_some_and(|(_, terminal)| terminal.grab_focus()) {
            if let Some(given) = &desired {
                self.given_focus.push(given.clone());
            }
            self.focused = desired;
        } else if desired.is_none() {
            self.focused = None;
        }
    }

    /// The panes given focus since the last take, as (session, pane token), in order: the frontend's own focus moves,
    /// which the window reports as [`crate::state::AppState::pane_focused`] (upstream's `becomeFirstResponder` fires
    /// for a programmatic focus as for a click, and an explicit refocus of the terminal that holds the keyboard sends
    /// GTK no focus-enter).
    pub fn take_given_focus(&mut self) -> Vec<(Uuid, String)> {
        std::mem::take(&mut self.given_focus)
    }

    /// Puts a fresh terminal in place of the session pane `replacement` names (upstream's `replacePane`): the same
    /// token, page position and visibility, the replacement's command over the pane's own environment, and the keyboard
    /// when the replaced one had it. The replaced terminal goes with none of the pane's close paths: its client's exit
    /// is claimed first. False when this deck has no such pane.
    pub fn replace_pane(
        &mut self,
        core: &mut Core,
        window: Uuid,
        replacement: &PaneReplacement,
        context: &PaneContext,
        poster: &Poster,
    ) -> bool {
        let (session, token) = (replacement.session, replacement.token.as_str());
        let Some(page) = self.pages.get_mut(&session) else {
            return false;
        };
        let Some(old) = page.panes.terminal(token).cloned() else {
            return false;
        };
        let Some(spec) = replacement_spec(core, window, session, token) else {
            return false;
        };
        let mut surface_spec = spec.surface_spec(context);
        surface_spec.command = Some(replacement.command.clone());
        // a remote replica's attach holds after its ssh, as the pane's first attach did
        surface_spec.wait_after_command = replacement.remote;
        surface_spec.env = surface::merged_env(&surface_spec.env, &replacement.env);
        surface_spec.live = !replacement.remote;
        surface_spec.working_directory = replacement
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| old.working_directory());
        #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
        if let Some(size) = old.font_size() {
            surface_spec.font_size = Some(size as f32);
        }
        let terminal = TerminalSurface::new(surface_spec);
        terminal.set_owner(SurfaceOwner::Pane {
            session,
            token: token.to_owned(),
        });
        connect_terminal(&terminal, session, token, poster);
        connect_reports(&terminal, session, token, poster);
        crate::pane_lead::connect_takeover(&terminal, session, token, poster);
        let had_focus = old.widget().has_focus();
        old.claim_exit();
        page.panes.replace(token, terminal.clone());
        let installed = core
            .library_mut()
            .store_mut(window)
            .and_then(|store| store.session_mut(session))
            .and_then(|found| Some((found.pane_role_for_token(token)?, found)))
            .map(|(role, found)| {
                found
                    .pane_slot_mut(role)
                    .replace(Box::new(PaneSurface::new(terminal.clone(), token.to_owned())))
            });
        if let Some(Some(previous)) = installed {
            previous.teardown();
        }
        old.teardown();
        if had_focus {
            terminal.widget().grab_focus();
        }
        true
    }
}

/// The pane spec of the session pane carrying `token` in window `window`: what a replacement inherits.
fn replacement_spec(core: &Core, window: Uuid, session: Uuid, token: &str) -> Option<PaneSpec> {
    let store = core.library().store(window)?;
    let found = store.session(session)?;
    match found.pane_role_for_token(token)? {
        StatusPane::Left => PaneSpec::primary(window, store, session),
        StatusPane::Right => PaneSpec::split(window, store.workspace_for_session(session)?.id(), found),
        StatusPane::Scratch => None,
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
/// split's focus there (unless the focus moved on before the task ran, or the pane is hidden), a key press clears
/// the session's status when this pane owns it (upstream's `wireStatusClear`), and a font size change persists when
/// the pane is in the main role (upstream's `onFontSizeChange` → `persistFontSize`).
fn connect_terminal(terminal: &TerminalSurface, session: Uuid, token: &str, poster: &Poster) {
    let posting = poster.clone();
    let font_token = token.to_owned();
    terminal.connect_font_size(move |size| {
        let token = font_token.clone();
        state::post(&posting, move |state: &mut AppState| {
            if state.core.report_font_size(session, &token, size) {
                state.settle();
            }
        });
    });
    let posting = poster.clone();
    let exit_token = token.to_owned();
    terminal.connect_exit(move || {
        state::post(&posting, move |state: &mut AppState| {
            state.pane_exited(session, &exit_token);
        });
    });
    let posting = poster.clone();
    let held_token = token.to_owned();
    terminal.connect_held(move || {
        state::post(&posting, move |state: &mut AppState| {
            if state.core.remote_pane_held(session, &held_token) {
                state.settle();
            }
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
    connect_notifications(terminal, session, token, poster);
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

/// Routes the terminal's desktop notifications (OSC 9/777) to the model, with whether this pane held its window's
/// keyboard and whether that window was active when it fired, sampled now (upstream's `NotificationManager.notify`
/// suppression: the first responder and `NSApp.isActive`).
fn connect_notifications(terminal: &TerminalSurface, session: Uuid, token: &str, poster: &Poster) {
    let posting = poster.clone();
    let token = token.to_owned();
    let area = terminal.widget().downgrade();
    terminal.connect_notification(move |title, body| {
        let area = area.upgrade();
        let focus = crate::notifications::Focus {
            pane: area.as_ref().is_some_and(WidgetExt::has_focus),
            active: area
                .and_then(|area| area.root())
                .and_downcast::<gtk::Window>()
                .is_some_and(|window| window.is_active()),
        };
        let (title, body, token) = (title.to_owned(), body.to_owned(), token.clone());
        state::post(&posting, move |state: &mut AppState| {
            state.terminal_notification(session, &token, &title, &body, focus);
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
            ghostty_app::follow_link_in(state, &url, ClickOrigin::Pane(session));
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

        // a refocus of the pane that holds the keyboard already sends GTK no focus-enter: the deck reports it itself,
        // so the session's banners are withdrawn all the same
        let _ = walk.deck.take_given_focus();
        assert!(walk.core.dispatch(&request).ok);
        walk.settle();
        assert!(split.is_focus());
        assert_eq!(walk.deck.take_given_focus(), [(walk.session, walk.split.clone())]);
        walk.core.clear_pane_focus_requests();
        walk.settle();
        assert!(
            walk.deck.take_given_focus().is_empty(),
            "an ordinary render gives no focus"
        );

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
        let _ = walk.deck.take_given_focus();
        walk.settle();
        assert!(walk.deck.in_grid);
        // a selection under the dashboard gives no pane focus, so the session's banners stay
        let session = walk.session;
        walk.store().select_session(Some(session));
        walk.settle();
        assert!(walk.deck.take_given_focus().is_empty());
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

    #[test]
    fn the_hud_panel_mounts_in_its_layer_and_goes_with_the_slot() {
        crate::gtk_test::on_gtk_thread(hud_mounts_in_its_layer);
    }

    /// A HUD's terminal is mounted into the page's layer, passive; a reopen gives a fresh terminal in the same layer,
    /// a close drops it, and the layer itself stays the stack's child throughout.
    fn hud_mounts_in_its_layer() {
        let mut walk = Walk::split();
        let dir = tempfile::tempdir().unwrap();
        walk.core
            .set_hud_helper(crate::hud::install_helper(dir.path()).unwrap(), dir.path().join("hud"));
        let open = |walk: &mut Walk, message: &str| {
            let request = Request {
                cmd: Command::Session(SessionCmd::HudOpen),
                target: Some(agterm_core::pane_env::uuid_string(walk.session)),
                args: Some(Args {
                    message: Some(message.to_owned()),
                    ..Args::default()
                }),
            };
            assert!(walk.core.dispatch(&request).ok);
            walk.settle();
        };
        open(&mut walk, "first");
        let layer = walk.deck.pages[&walk.session].panes.hud_mut().widget().clone();
        assert_eq!(layer.parent().as_ref(), Some(&walk.stack));
        let first = walk.deck.pages[&walk.session]
            .panes
            .hud_mut()
            .terminal()
            .unwrap()
            .widget()
            .clone();
        assert_eq!(first.parent().and_upcast::<gtk::Widget>(), Some(layer.clone().upcast()));
        assert!(!first.is_focusable() && !first.can_target(), "passive");
        assert!(
            walk.probe().terminal(&walk.split).widget().is_focusable(),
            "the pane keeps the keyboard"
        );

        open(&mut walk, "second");
        let second = walk.deck.pages[&walk.session]
            .panes
            .hud_mut()
            .terminal()
            .unwrap()
            .widget()
            .clone();
        assert_ne!(first, second, "a replacement is a fresh terminal");
        assert!(first.parent().is_none());

        let close = Request {
            cmd: Command::Session(SessionCmd::HudClose),
            target: Some(agterm_core::pane_env::uuid_string(walk.session)),
            args: None,
        };
        assert!(walk.core.dispatch(&close).ok);
        walk.settle();
        assert!(walk.deck.pages[&walk.session].panes.hud_mut().terminal().is_none());
        assert!(second.parent().is_none());
        assert_eq!(layer.parent().as_ref(), Some(&walk.stack));
    }

    #[test]
    fn overlays_mount_in_place_cover_and_take_the_keyboard() {
        crate::gtk_test::on_gtk_thread(overlays_mount_in_place);
    }

    fn overlay_request(walk: &Walk, cmd: SessionCmd, edit: impl FnOnce(&mut Args)) -> Request {
        let mut args = Args::default();
        edit(&mut args);
        Request {
            cmd: Command::Session(cmd),
            target: Some(agterm_core::pane_env::uuid_string(walk.session)),
            args: Some(args),
        }
    }

    /// A pane overlay's terminal is a child of the stack over its pane, which hides under it, and takes the focused
    /// pane's keyboard; a covering program's terminal sits in its layer, hides both panes and takes the keyboard; each
    /// close drops its terminal and gives the pane back.
    fn overlays_mount_in_place() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_child(Some(walk.deck.widget()));
        let pane_overlay = pane_overlay_covers_its_pane(&mut walk);
        let program = program_covers_the_page(&mut walk, &pane_overlay);
        assert!(
            walk.core
                .dispatch(&overlay_request(&walk, SessionCmd::OverlayClose, |_| {}))
                .ok
        );
        walk.settle();
        assert!(program.widget().parent().is_none());
        assert!(walk.deck.pages[&walk.session].panes.program().terminal().is_none());
        let close_right = overlay_request(&walk, SessionCmd::OverlayClose, |a| a.pane = Some("right".to_owned()));
        assert!(walk.core.dispatch(&close_right).ok);
        walk.settle();
        assert!(pane_overlay.widget().parent().is_none());
        assert_eq!(walk.shown(), (true, true));
        assert_eq!(walk.probe().focus(), Some(walk.split.clone()));
        assert!(
            walk.probe().area(&walk.split).is_focus(),
            "the pane got the keyboard back"
        );
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    fn pane_overlay_covers_its_pane(walk: &mut Walk) -> TerminalSurface {
        let open_right = overlay_request(walk, SessionCmd::OverlayOpen, |a| {
            a.command = Some("exec cat".to_owned());
            a.pane = Some("right".to_owned());
        });
        assert!(walk.core.dispatch(&open_right).ok);
        walk.settle();
        let session = walk.session;
        let generation = walk
            .store()
            .session(session)
            .unwrap()
            .pane_overlay(agterm_core::model::OverlayPane::Right)
            .unwrap()
            .generation;
        let page = &walk.deck.pages[&walk.session].panes;
        let pane_overlay = page.pane_overlay(generation).unwrap().terminal.clone();
        assert_eq!(pane_overlay.widget().parent().as_ref(), Some(&walk.stack));
        assert!(pane_overlay.is_deck_visible());
        assert_eq!(walk.shown(), (true, false), "the right pane hides under its overlay");
        assert_eq!(walk.probe().focus(), Some(format!("overlay-pane#{generation}")));
        assert!(
            pane_overlay.widget().is_focus(),
            "GTK gave the pane overlay the keyboard"
        );
        pane_overlay
    }

    fn program_covers_the_page(walk: &mut Walk, pane_overlay: &TerminalSurface) -> TerminalSurface {
        let open_program = overlay_request(walk, SessionCmd::OverlayOpen, |a| {
            a.command = Some("exec cat".to_owned());
        });
        assert!(walk.core.dispatch(&open_program).ok);
        walk.settle();
        let layer = walk.deck.pages[&walk.session].panes.program();
        let frame = layer.frame().clone();
        let program = layer.terminal().unwrap().clone();
        assert_eq!(
            program.widget().parent().and_upcast::<gtk::Widget>(),
            Some(frame.clone().upcast())
        );
        assert_eq!(frame.parent().as_ref(), Some(&walk.stack));
        assert_eq!(walk.shown(), (false, false), "a covering program hides the panes");
        assert!(!pane_overlay.is_deck_visible(), "and the pane overlay");
        assert_eq!(walk.probe().focus(), Some("overlay#1".to_owned()));
        assert!(
            program.widget().is_focus(),
            "GTK gave the program the keyboard through its frame"
        );
        program
    }

    #[cfg(feature = "html")]
    #[test]
    fn a_soft_closed_sessions_page_is_off_screen_until_the_undo() {
        crate::gtk_test::on_gtk_thread(soft_closed_page_is_off_screen);
    }

    /// A shown page is on screen; once a soft close holds its session the page stays but is off screen, so a link a
    /// script clicks asks nothing over the session shown next; the undo puts it back on screen.
    #[cfg(feature = "html")]
    fn soft_closed_page_is_off_screen() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.html");
        std::fs::write(&file, "<p>page</p>").unwrap();
        let mut walk = Walk::split();
        crate::html_overlay::install(&mut walk.core, dir.path().join("state"));
        let open = overlay_request(&walk, SessionCmd::OverlayOpen, |a| {
            a.html = Some(file.display().to_string());
        });
        let response = walk.core.dispatch(&open);
        assert!(response.ok, "{:?}", response.error);
        walk.settle();
        let session = walk.session;
        let page = walk.store().session(session).unwrap().overlay.page().unwrap().id;
        assert_eq!(crate::html_overlay::on_screen(page), Some(true));

        assert!(walk.store().soft_close_session(session));
        reconcile(&mut walk.deck, &mut walk.core, walk.window);
        assert_eq!(
            crate::html_overlay::on_screen(page),
            Some(false),
            "the held session's page is off screen"
        );

        assert!(walk.store().undo_pending_close(None));
        walk.settle();
        assert_eq!(crate::html_overlay::on_screen(page), Some(true));
    }

    #[test]
    fn overlay_terminals_take_the_pointer_and_a_hud_never_does() {
        crate::gtk_test::on_gtk_thread(overlays_take_the_pointer);
    }

    /// Runs the GTK main context until `done` holds (layout and mapping happen on the frame clock) or a few seconds
    /// pass; whether it held.
    fn pump_until(mut done: impl FnMut() -> bool) -> bool {
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }

    /// What a click at the center of `target` reaches in `window` (GTK's hit-testing, which skips a widget that is
    /// not targetable together with everything under it), once the layout gives `expected` (or a few seconds pass).
    fn pick_center(
        window: &gtk::Window,
        target: &impl IsA<gtk::Widget>,
        expected: impl Fn(Option<&gtk::Widget>) -> bool,
    ) -> Option<gtk::Widget> {
        let mut picked = None;
        pump_until(|| {
            picked = target
                .compute_bounds(window)
                .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
                .and_then(|rect| {
                    window.pick(
                        f64::from(rect.x() + rect.width() / 2.0),
                        f64::from(rect.y() + rect.height() / 2.0),
                        gtk::PickFlags::DEFAULT,
                    )
                });
            expected(picked.as_ref())
        });
        picked
    }

    /// Whether a pick reached `widget`.
    fn reached(widget: &impl IsA<gtk::Widget>) -> impl Fn(Option<&gtk::Widget>) -> bool + '_ {
        move |picked| picked == Some(widget.upcast_ref())
    }

    /// A shown overlay's terminal is what a click over it reaches (its clicks, scrolls and motion go to the program),
    /// covering, floating or over a pane; a floating panel's backdrop takes the clicks around it; a HUD's terminal
    /// never takes one.
    fn overlays_take_the_pointer() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(walk.deck.widget()));
        window.present();
        pump_until(|| walk.stack.is_mapped() && walk.stack.width() > 0);
        pane_overlay_takes_the_pointer(&mut walk, &window);
        program_takes_the_pointer(&mut walk, &window);
        hud_lets_the_pointer_through(&mut walk, &window);
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    fn pane_overlay_takes_the_pointer(walk: &mut Walk, window: &gtk::Window) {
        let open_right = overlay_request(walk, SessionCmd::OverlayOpen, |a| {
            a.command = Some("exec cat".to_owned());
            a.pane = Some("right".to_owned());
        });
        assert!(walk.core.dispatch(&open_right).ok);
        walk.settle();
        let session = walk.session;
        let generation = walk
            .store()
            .session(session)
            .unwrap()
            .pane_overlay(agterm_core::model::OverlayPane::Right)
            .unwrap()
            .generation;
        let pane_overlay = walk.deck.pages[&session]
            .panes
            .pane_overlay(generation)
            .unwrap()
            .terminal
            .clone();
        let pane_overlay = pane_overlay.widget().clone();
        assert_eq!(
            pick_center(window, &pane_overlay, reached(&pane_overlay)).as_ref(),
            Some(pane_overlay.upcast_ref()),
            "a click over the right pane reaches its overlay"
        );
        let primary = walk.probe().area(&walk.primary);
        assert_eq!(
            pick_center(window, &primary, reached(&primary)).as_ref(),
            Some(primary.upcast_ref()),
            "the left pane keeps its clicks"
        );
    }

    fn program_takes_the_pointer(walk: &mut Walk, window: &gtk::Window) {
        let session = walk.session;
        let full = overlay_request(walk, SessionCmd::OverlayOpen, |a| {
            a.command = Some("exec cat".to_owned());
            a.full = Some(true);
        });
        assert!(walk.core.dispatch(&full).ok);
        walk.settle();
        let program = walk.deck.pages[&session]
            .panes
            .program()
            .terminal()
            .unwrap()
            .widget()
            .clone();
        let stack = walk.stack.clone();
        assert_eq!(
            pick_center(window, &stack, reached(&program)).as_ref(),
            Some(program.upcast_ref()),
            "a covering program takes the clicks over the page"
        );

        let floating = overlay_request(walk, SessionCmd::OverlayResize, |a| a.size_percent = Some(50));
        assert!(walk.core.dispatch(&floating).ok);
        walk.settle();
        let backdrop: gtk::Widget = walk.deck.pages[&session].panes.program().backdrop().clone().upcast();
        let corner = || {
            let origin = stack.compute_point(window, &gtk::graphene::Point::new(4.0, 4.0))?;
            window.pick(f64::from(origin.x()), f64::from(origin.y()), gtk::PickFlags::DEFAULT)
        };
        pump_until(|| corner().as_ref() == Some(&backdrop));
        assert_eq!(
            corner(),
            Some(backdrop),
            "a floating program's backdrop absorbs the clicks around it"
        );
        assert_eq!(
            pick_center(window, &stack, reached(&program)).as_ref(),
            Some(program.upcast_ref()),
            "a floating program takes the clicks over itself"
        );
    }

    fn hud_lets_the_pointer_through(walk: &mut Walk, window: &gtk::Window) {
        let session = walk.session;
        let stack = walk.stack.clone();
        assert!(
            walk.core
                .dispatch(&overlay_request(walk, SessionCmd::OverlayClose, |_| {}))
                .ok
        );
        walk.settle();
        let dir = tempfile::tempdir().unwrap();
        walk.core
            .set_hud_helper(crate::hud::install_helper(dir.path()).unwrap(), dir.path().join("hud"));
        let hud = overlay_request(walk, SessionCmd::HudOpen, |a| a.message = Some("passive".to_owned()));
        assert!(walk.core.dispatch(&hud).ok);
        walk.settle();
        let hud = walk.deck.pages[&session]
            .panes
            .hud_mut()
            .terminal()
            .unwrap()
            .widget()
            .clone();
        let layer: gtk::Widget = walk.deck.pages[&session].panes.hud_mut().widget().clone().upcast();
        let laid_out = || hud.compute_bounds(window).is_some_and(|rect| rect.width() > 0.0);
        assert!(pump_until(laid_out), "the HUD is laid out");
        let under = pick_center(window, &hud, |_| true).expect("a click lands on the page");
        assert!(
            under != *hud.upcast_ref::<gtk::Widget>() && under != layer && under.is_ancestor(&stack),
            "a HUD lets the clicks through to the page under it: {under:?}"
        );
    }

    #[test]
    fn a_terminal_ask_takes_the_keyboard_in_its_layer() {
        crate::gtk_test::on_gtk_thread(terminal_ask_takes_the_keyboard);
    }

    /// A terminal ask over the focused split pane mounts its dialog in the page's ask layer, which stays the stack's
    /// child, and takes the keyboard from the pane; once answered the pane gets the keyboard back.
    fn terminal_ask_takes_the_keyboard() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_child(Some(walk.deck.widget()));
        walk.settle();
        let split = walk.probe().area(&walk.split);
        assert!(split.is_focus());
        let open = Request {
            cmd: Command::Ask(agterm_proto::command::AskCmd::Open),
            target: Some(pane_env::uuid_string(walk.session)),
            args: Some(Args {
                title: Some("Deploy?".to_owned()),
                buttons: Some(vec![agterm_proto::AskButton {
                    id: "yes".to_owned(),
                    label: "Yes".to_owned(),
                    hotkey: None,
                }]),
                pane: Some("right".to_owned()),
                ..Args::default()
            }),
        };
        let response = walk.core.dispatch(&open);
        assert!(response.ok, "{response:?}");
        walk.settle();
        let layer = walk.deck.pages[&walk.session].panes.ask().widget().clone();
        assert_eq!(layer.parent().and_upcast::<gtk::Widget>(), Some(walk.stack.clone()));
        assert!(layer.get_visible());
        let (_, dialog) = walk.deck.pages[&walk.session].panes.ask().focus_target().unwrap();
        assert!(dialog.is_focus(), "the dialog holds the keyboard");
        assert!(!split.is_focus());

        let session = walk
            .core
            .library()
            .store(walk.window)
            .unwrap()
            .session(walk.session)
            .unwrap();
        let id = session.ask().unwrap().ask.id.clone();
        let escaped = agterm_proto::ControlAskResult::outcome(agterm_proto::ControlAskOutcome::Escaped);
        walk.core.answer_session_ask(walk.session, &id, escaped);
        walk.settle();
        assert!(!layer.get_visible());
        assert!(dialog.parent().is_none(), "the dialog went");
        assert_eq!(layer.parent().and_upcast::<gtk::Widget>(), Some(walk.stack.clone()));
        assert!(split.is_focus(), "the pane got the keyboard back");
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    #[test]
    fn a_window_modal_holds_the_terminals_keyboard() {
        crate::gtk_test::on_gtk_thread(modal_holds_the_keyboard);
    }

    /// While the window's picker is up no terminal is given the keyboard, not even by a focus request; once it is
    /// answered the shown pane is.
    fn modal_holds_the_keyboard() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_child(Some(walk.deck.widget()));
        walk.settle();
        let split = walk.probe().area(&walk.split);
        GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        let open = Request {
            cmd: Command::Pick(agterm_proto::command::PickCmd::Open),
            target: None,
            args: Some(Args {
                items: Some(vec![agterm_proto::PickItem {
                    id: "a".to_owned(),
                    label: "Alpha".to_owned(),
                    subtitle: None,
                }]),
                ..Args::default()
            }),
        };
        let response = walk.core.dispatch(&open);
        assert!(response.ok, "{response:?}");
        let focus = Request {
            cmd: Command::Session(SessionCmd::Focus),
            target: Some(pane_env::uuid_string(walk.session)),
            args: Some(Args {
                pane: Some("right".to_owned()),
                ..Args::default()
            }),
        };
        assert!(walk.core.dispatch(&focus).ok);
        walk.settle();
        assert!(!split.is_focus(), "no terminal takes the keyboard under the picker");
        walk.core.clear_pane_focus_requests();
        let id = walk.core.modal_slot(walk.window).unwrap().pending().unwrap().id.clone();
        let cancelled = agterm_proto::ControlPickResult::outcome(agterm_proto::ControlPickOutcome::Cancelled);
        walk.core.resolve_pick(walk.window, &id, cancelled);
        walk.settle();
        assert!(split.is_focus(), "the shown pane gets the keyboard back");
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    #[test]
    fn a_window_modal_holds_the_keyboard_over_the_dashboard() {
        crate::gtk_test::on_gtk_thread(modal_holds_the_keyboard_over_the_dashboard);
    }

    /// A picker opened over the dashboard keeps the keyboard through every later reconcile (a blocking `pick` polls,
    /// and each poll settles); once it is answered the dashboard's chrome takes the keyboard back.
    fn modal_holds_the_keyboard_over_the_dashboard() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        window.set_child(Some(walk.deck.widget()));
        walk.settle();
        let dashboard = Request {
            cmd: Command::Dashboard,
            target: None,
            args: Some(Args {
                targets: Some(vec![pane_env::uuid_string(walk.session)]),
                ..Args::default()
            }),
        };
        assert!(walk.core.dispatch(&dashboard).ok);
        walk.settle();
        let chrome = walk.deck.chrome().widget().clone();
        assert!(chrome.is_focus(), "the open dashboard's chrome holds the keyboard");
        let open = Request {
            cmd: Command::Pick(agterm_proto::command::PickCmd::Open),
            target: None,
            args: Some(Args {
                items: Some(vec![agterm_proto::PickItem {
                    id: "a".to_owned(),
                    label: "Alpha".to_owned(),
                    subtitle: None,
                }]),
                ..Args::default()
            }),
        };
        let response = walk.core.dispatch(&open);
        assert!(response.ok, "{response:?}");
        // the picker takes the keyboard (the window does that; here nothing holds it)
        GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        walk.settle();
        walk.settle();
        assert!(!chrome.is_focus(), "the chrome leaves the keyboard to the picker");
        let id = walk.core.modal_slot(walk.window).unwrap().pending().unwrap().id.clone();
        let cancelled = agterm_proto::ControlPickResult::outcome(agterm_proto::ControlPickOutcome::Cancelled);
        walk.core.resolve_pick(walk.window, &id, cancelled);
        walk.settle();
        assert!(chrome.is_focus(), "the chrome gets the keyboard back");
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
    }

    #[test]
    fn a_window_modal_keeps_the_keyboard_when_the_last_session_goes() {
        crate::gtk_test::on_gtk_thread(modal_keeps_the_keyboard_when_the_last_session_goes);
    }

    /// The window's last session going while its picker is up leaves the picker the keyboard (the deck drops focus
    /// to nothing only when no modal holds it); a session shown after the picker went gets the keyboard again.
    fn modal_keeps_the_keyboard_when_the_last_session_goes() {
        let mut walk = Walk::split();
        let window = gtk::Window::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let picker = gtk::Button::with_label("picker");
        picker.set_focusable(true);
        content.append(walk.deck.widget());
        content.append(&picker);
        window.set_child(Some(&content));
        walk.settle();
        let split = walk.probe().area(&walk.split);
        assert!(split.is_focus());
        assert!(
            picker.grab_focus(),
            "the picker takes the keyboard (the window does that)"
        );
        walk.deck.show(None, false, true);
        assert!(picker.is_focus(), "the picker keeps the keyboard with no session shown");
        walk.deck.show(Some(walk.session), false, false);
        assert!(split.is_focus(), "the next shown session gets the keyboard");
        walk.deck.show(None, false, false);
        assert!(
            !split.is_focus() && !picker.is_focus(),
            "with no modal the focus leaves the hidden terminals"
        );
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
