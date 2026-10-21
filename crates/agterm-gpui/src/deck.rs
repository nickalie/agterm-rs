//! The detail deck: every session of the window holds one page with its terminals, all mounted at once, and only the
//! selected one visible, hit-testable and focused (upstream's eager deck, `WindowContentView+Detail.swift`
//! `detailPane`; ported from the GTK build's `deck.rs`). A session made at runtime spawns its shell as soon as its
//! terminal is laid out; a restored one waits until it is first shown, as the GTK fork's deck does (the core's lazy
//! restore, `agterm_core::commands::relaunch`).
//!
//! A page is created once and keeps its terminals' views: switching sessions is a visibility flip. A hidden page is
//! laid out under the shown one but not drawn ([`TerminalView::set_shown`]), so its terminals still start at their
//! size and follow it: a runtime session's shells run (the conformance harness's `wait_mounted` waits for a `noSelect`
//! session's `realized` too), while a dormant restored session's terminals wait for their seed until it wakes.
//! Sessions a soft close holds out of the tree keep their pages, hidden, until the close is final. A page goes once
//! its session is gone; closing the session already tore its terminals down. Inside a page the session's panes follow
//! [`crate::split`].
//!
//! The model moves keyboard focus only by what it says should hold it: the deck focuses the shown page's focused pane
//! whenever that pane (or the shown page) differs from what it last focused, which is how selecting a session,
//! `session.focus`, a swap or the scratch move the input. A terminal the user focuses reports back through
//! [`AppState::pane_focused`]. While the window's picker or GUI ask, an alert or a popover is up no terminal is given
//! the keyboard, and the shown terminal gets it back once it closes. A session's terminal ask that wants the keyboard
//! takes it in place of the terminal it covers ([`crate::ask_dialog`]), and gives it back once it ends.
//!
//! Over a page's panes lie its pane overlays and its program overlay ([`crate::overlay`]), then its HUD
//! ([`crate::hud`]) and, above that, its terminal ask. An overlay the layout gives the keyboard takes it in place of
//! the pane it covers, and gives it back once it goes.
//!
//! While the window's dashboard is open no page shows: the grid ([`crate::dashboard`]) covers the deck and hosts each
//! member pane's own terminal view in its cell, which its page then does not draw, and the grid's focus holds the
//! keyboard. Once it closes the member terminals go back to their pages at their own font size, and the shown page's
//! terminal takes the keyboard again.
//!
//! The plan of what to add, remove and show is a pure function of the model ([`plan`]), so it is testable without a
//! window.
//!
//! [`TerminalView::set_shown`]: crate::terminal::TerminalView::set_shown

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use agterm_core::Core;
use agterm_core::chrome::Rgb;
use agterm_core::keymap::SurfaceOwner;
use agterm_core::model::pane::SPLIT_RATIO_DEFAULT;
use agterm_core::model::{Session, StatusPane, WindowLibrary, Workspace};
use agterm_core::restore::spawn_plan::LaunchSeed;
use agterm_core::zoom::SurfaceId;
use agterm_core::{PaneReplacement, PaneSpawn};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, CursorStyle, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, point, px, size,
};
use uuid::Uuid;

use crate::ask_dialog::{self, AskDialog, AskOwner};
use crate::hud::{self, HudLayer};
use crate::mute::{self, Washes};
use crate::overlay::{self, OverlayLayer};
use crate::pane::{self, PaneContext, PaneSpec, Ran, RanCell};
use crate::pane_lead::{self, PaneCover};
use crate::split::{self, AskScope, PageElement, PageState, PanePlan, Place, Slot};
use crate::state::{AppState, NotifyFocus, Poster, post};
use crate::terminal::{KeyHold, Shared, Terminal, TerminalConfig, TerminalEvent, TerminalView, ViewRole};
use crate::watermark;

/// What upstream's deck shows with no session selected.
pub const NO_SESSION: &str = "No session selected";

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
pub fn plan<S: BuildHasher>(library: &WindowLibrary, window: Option<Uuid>, mounted: &HashSet<Uuid, S>) -> DeckPlan {
    let Some((window, store)) = window.and_then(|id| library.store(id).map(|store| (id, store))) else {
        let mut remove: Vec<Uuid> = mounted.iter().copied().collect();
        remove.sort_unstable();
        return DeckPlan {
            remove,
            ..DeckPlan::default()
        };
    };
    let held = store.pending_held_session_ids();
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

/// What a GUI ask covers (upstream's `askAnchorFrame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorTarget {
    /// The terminal area: an ask with no anchor.
    Area,
    /// An anchored session's page, or its pane at the slot.
    Page { session: Uuid, slot: Option<Slot> },
}

/// What GUI ask `ask` of window `window` covers: the terminal area without an anchor, else its session's page or that
/// page's pane; `None` while the pane is gone (the core cancels the ask then).
#[must_use]
pub fn anchor_target(core: &Core, window: Uuid, ask: &agterm_core::model::PendingAsk) -> Option<AnchorTarget> {
    let Some(anchor) = &ask.anchor else {
        return Some(AnchorTarget::Area);
    };
    let slot = match anchor.pane_identity {
        None => None,
        Some(identity) => {
            let session = core.library().store(window)?.session(anchor.session)?;
            Some(Slot::of_overlay(session.pane_role_for_identity(identity)?))
        }
    };
    Some(AnchorTarget::Page {
        session: anchor.session,
        slot,
    })
}

/// The app's config with a terminal's own overlay loaded last (a session or pane background).
pub type ConfigWith = Rc<dyn Fn(&str) -> TerminalConfig>;

/// Where the session and pane backgrounds go: the state dir their text images are drawn under, and the config a
/// terminal with an overlay takes.
pub struct Watermarks {
    pub state_dir: std::path::PathBuf,
    pub config_with: ConfigWith,
}

/// What every deck of the app shares: how panes start, what their shells are told, the config they draw with, the
/// main loop.
pub struct DeckEnv {
    pub context: PaneContext,
    pub launcher: pane::Launcher,
    pub config: RefCell<Rc<TerminalConfig>>,
    pub poster: Poster,
    /// This machine's name, which a working-directory report's host must match.
    pub hostname: String,
    pub watermarks: Watermarks,
}

impl DeckEnv {
    #[must_use]
    pub fn new(
        context: PaneContext,
        launcher: pane::Launcher,
        config: TerminalConfig,
        poster: Poster,
        watermarks: Watermarks,
    ) -> Self {
        Self {
            context,
            launcher,
            config: RefCell::new(Rc::new(config)),
            poster,
            hostname: agterm_term::pwd::local_hostname().unwrap_or_default(),
            watermarks,
        }
    }

    /// The config a terminal with `overlay` (none: the app's own) takes.
    #[must_use]
    pub fn config_for(&self, overlay: Option<&str>) -> TerminalConfig {
        match overlay.filter(|overlay| !overlay.is_empty()) {
            Some(overlay) => (self.watermarks.config_with)(overlay),
            None => (*self.config()).clone(),
        }
    }

    /// The config terminals start with.
    #[must_use]
    pub fn config(&self) -> Rc<TerminalConfig> {
        Rc::clone(&self.config.borrow())
    }
}

/// The colors the deck's own drawing takes from the terminals' theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub background: Rgb,
    pub foreground: Rgb,
    /// The inactive pane and backdrop mute's opacity.
    pub mute: f64,
}

impl Look {
    /// The deck's colors in a window rendering at `window_opacity` (1, or the translucency's below it): the mute wash
    /// is scaled by it, as upstream's `muteWashOpacity` is, so a muted pane over a translucent backing stays as see-through
    /// as the title bar beside it.
    #[must_use]
    pub fn of(config: &TerminalConfig, core: &Core, window_opacity: f64) -> Self {
        let rgb = |color: agterm_term::Rgb| Rgb::new(color.r, color.g, color.b);
        Self {
            background: rgb(config.theme.background),
            foreground: rgb(config.theme.foreground),
            mute: core.settings().inactive_pane_mute_opacity() * window_opacity,
        }
    }
}

/// One terminal of a page and the token its shell carries.
pub struct PaneTerminal {
    pub spec: PaneSpec,
    pub terminal: Entity<Terminal>,
    pub view: Entity<TerminalView>,
    shared: Rc<Shared>,
    ran: RanCell,
    /// The key press that a lead cover or reconnect wait asks for, and whether a held exit closes on the next key.
    cover_holds: Rc<Cell<bool>>,
    exit_held: Rc<Cell<bool>>,
    /// The terminal's own config overlay (its background), loaded over the app's config.
    overlay: RefCell<Option<String>>,
    /// The background a program set with OSC 11 while it shows, and the window opacity its overlay lifts the
    /// terminal's `background-opacity` to (upstream's `oscBackgroundColorHex` and `applyOSCBackground`): under window
    /// translucency the terminals render transparent, which would hide it.
    osc_background: RefCell<Option<(String, f64)>>,
    _subscriptions: Vec<Subscription>,
}

impl PaneTerminal {
    /// The overlay the terminal renders with: the OSC 11 lift while a program's background shows, else its own.
    fn effective_overlay(&self) -> Option<String> {
        match &*self.osc_background.borrow() {
            Some((_, opacity)) => Some(agterm_core::watermark::osc_background_overlay_text(None, *opacity)),
            None => self.overlay.borrow().clone(),
        }
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.spec.token
    }

    /// The view's focus handle.
    #[must_use]
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.read(cx).focus_handle(cx)
    }

    /// The state the terminal, its views and its surface share.
    #[must_use]
    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }
}

/// A page's terminal ask: its dialog, what it covers, and whether it takes the keyboard.
pub struct PageAsk {
    pub dialog: Entity<AskDialog>,
    pub scope: AskScope,
    pub wants_focus: bool,
}

/// One session's page: its terminals, their layout, and what lies over them.
pub struct Page {
    session: Uuid,
    panes: Vec<PaneTerminal>,
    plan: Option<PanePlan>,
    /// The lead covers by pane token.
    covers: Vec<(String, PaneCover)>,
    washes: Washes,
    state: Rc<PageState>,
    hud: HudLayer,
    overlays: OverlayLayer,
    ask: Option<PageAsk>,
}

impl Page {
    fn new(session: Uuid) -> Self {
        Self {
            session,
            panes: Vec::new(),
            plan: None,
            covers: Vec::new(),
            washes: Washes::default(),
            state: Rc::new(PageState::default()),
            hud: HudLayer::default(),
            overlays: OverlayLayer::default(),
            ask: None,
        }
    }

    /// The page's HUD.
    #[must_use]
    pub fn hud(&self) -> &HudLayer {
        &self.hud
    }

    /// The page's overlay terminals.
    #[must_use]
    pub fn overlays(&self) -> &OverlayLayer {
        &self.overlays
    }

    /// What takes the keyboard on this page by the last plan, with its focus key: the terminal ask that wants it, the
    /// overlay the plan focuses, else the focused pane's terminal.
    fn keyboard(&self, cx: &App) -> Option<(String, FocusHandle)> {
        if let Some(ask) = self.ask_focus(cx) {
            return Some(ask);
        }
        if let Some(overlay) = self.plan.as_ref().and_then(|plan| self.overlays.focus(plan, cx)) {
            return Some(overlay);
        }
        self.focus()
            .map(|pane| (pane.token().to_owned(), pane.focus_handle(cx)))
    }

    /// The page's terminal ask while it shows.
    #[must_use]
    pub fn ask(&self) -> Option<&PageAsk> {
        self.ask.as_ref()
    }

    /// The terminal ask that takes the keyboard in place of the terminal it covers, keyed by its ask.
    fn ask_focus(&self, cx: &App) -> Option<(String, FocusHandle)> {
        let ask = self.ask.as_ref().filter(|ask| ask.wants_focus)?;
        let dialog = ask.dialog.read(cx);
        Some((format!("ask:{}", dialog.id()), dialog.focus_handle().clone()))
    }

    /// The pane's rectangle at `slot` in the page as last laid out (the page itself for `None`).
    fn rect(&self, slot: Option<Slot>) -> Option<split::Rect> {
        let (width, height) = self.state.size.get();
        let Some(slot) = slot else {
            return Some(split::Rect::full(width, height));
        };
        let plan = self.plan.as_ref()?;
        let geometry = split::geometry(plan.panes, plan.axis, self.state.ratio(plan.ratio), width, height);
        split::pane_rect(plan.panes, &geometry, slot)
    }

    /// The terminal carrying `token`.
    #[must_use]
    pub fn terminal(&self, token: &str) -> Option<&PaneTerminal> {
        self.panes.iter().find(|pane| pane.token() == token)
    }

    /// The pane terminals, in mount order.
    #[must_use]
    pub fn panes(&self) -> &[PaneTerminal] {
        &self.panes
    }

    /// The layout of the last sync.
    #[must_use]
    pub fn plan(&self) -> Option<&PanePlan> {
        self.plan.as_ref()
    }

    /// The washes of the last sync.
    #[must_use]
    pub fn washes(&self) -> &Washes {
        &self.washes
    }

    /// The cover over the pane carrying `token` at the last sync.
    #[must_use]
    pub fn cover(&self, token: &str) -> Option<&PaneCover> {
        self.covers
            .iter()
            .find(|(known, _)| known == token)
            .map(|(_, cover)| cover)
    }

    /// The focused pane's terminal by the last plan. A terminal ask or an overlay may hold the keyboard over it (see
    /// `keyboard`).
    #[must_use]
    pub fn focus(&self) -> Option<&PaneTerminal> {
        let plan = self.plan.as_ref()?;
        self.terminal(plan.focus_token()?)
    }

    /// Hides every terminal: the session is out of the tree, held by a pending close, or the page is not shown.
    fn hide(&mut self, cx: &mut App) {
        for pane in &self.panes {
            pane.view.update(cx, |view, cx| view.set_shown(false, cx));
        }
        self.overlays.hide(cx);
        self.washes = Washes::default();
    }

    /// Shows the terminals `plan` draws while the page is `shown`, and hides the rest.
    fn show(&self, shown: bool, cx: &mut App) {
        for pane in &self.panes {
            let visible = shown && self.plan.as_ref().is_some_and(|plan| plan.draws(pane.token()));
            pane.view.update(cx, |view, cx| view.set_shown(visible, cx));
        }
    }

    /// Drops the terminals whose token `live` refuses: the model tore them down already (a closed split or scratch,
    /// a primary a promotion replaced), so this only lets their entities go.
    fn retain(&mut self, live: impl Fn(&str) -> bool) {
        self.panes.retain(|pane| live(pane.token()));
    }
}

/// The deck view of one window.
pub struct Deck {
    window: Uuid,
    env: Rc<DeckEnv>,
    pages: HashMap<Uuid, Page>,
    /// The pages in mount order, which is their drawing order.
    order: Vec<Uuid>,
    shown: Option<Uuid>,
    /// The page and pane token keyboard focus was last moved to.
    focused: Option<(Uuid, String)>,
    /// Counts the changes of `focused` and whether the user's last focus move awaits its report.
    focus_moves: Arc<FocusMoves>,
    /// A popup of the window's (the sidebar's row menu) holds the keyboard: the deck moves no focus meanwhile.
    held: bool,
    /// A focus move waited for the popup to close.
    held_move: bool,
    /// What may keep the keyboard a held move waited on: the sidebar's live rename field.
    owner: Option<FocusHandle>,
    look: Look,
    /// The deck's bounds in the window at the last layout, which a GUI ask is anchored in.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The window's open dashboard, as last reconciled.
    grid: Option<Grid>,
    /// The keyboard's holder while the dashboard is open (upstream's key catcher), made once.
    grid_focus: Option<FocusHandle>,
    /// The dashboard's transient font sizes on its member terminals.
    fonts: crate::dashboard::FontOverrides,
    /// The session whose terminal the window is zoomed into, as last reconciled.
    zoomed: Option<Uuid>,
}

/// An open dashboard as the deck draws it: the cells and the terminal each hosts.
struct Grid {
    cells: Vec<crate::dashboard::ChromeCell>,
    /// The (session, token) of each cell's terminal, in cell order.
    terminals: Vec<Option<(Uuid, String)>>,
}

/// The deck's focus moves as its posted focus reports see them. A user focus report posted before a later move is
/// stale when it runs, and is dropped: two queued reports would otherwise hand the model's focus back and forth. While
/// the user's last move awaits its report, the deck keeps it over the model's older focus.
#[derive(Default)]
struct FocusMoves {
    count: AtomicU64,
    reporting: AtomicBool,
}

/// What every page's reconcile shares.
struct PageSync {
    window: Uuid,
    zoom: Option<SurfaceId>,
    look: Look,
    /// The session whose page shows.
    shown: Option<Uuid>,
    /// Something else holds the keyboard: the window's picker, GUI ask, palette, an alert or a popover, or the quick
    /// terminal's panel.
    focus_blocked: bool,
    ask_look: ask_dialog::Look,
}

impl Deck {
    #[must_use]
    pub fn new(window: Uuid, env: Rc<DeckEnv>) -> Self {
        let look = Look {
            background: Rgb::BLACK,
            foreground: Rgb::WHITE,
            mute: 0.0,
        };
        Self {
            window,
            env,
            pages: HashMap::new(),
            order: Vec::new(),
            shown: None,
            focused: None,
            focus_moves: Arc::default(),
            held: false,
            held_move: false,
            owner: None,
            look,
            bounds: Rc::default(),
            grid: None,
            grid_focus: None,
            fonts: crate::dashboard::FontOverrides::default(),
            zoomed: None,
        }
    }

    /// Whether the window's dashboard is open.
    #[must_use]
    pub fn grid_open(&self) -> bool {
        self.grid.is_some()
    }

    /// The (session, token) of each open dashboard cell's terminal, in cell order.
    #[must_use]
    pub fn grid_terminals(&self) -> Vec<Option<(Uuid, String)>> {
        self.grid
            .as_ref()
            .map(|grid| grid.terminals.clone())
            .unwrap_or_default()
    }

    /// The dashboard's keyboard holder, once it has been open.
    #[must_use]
    pub fn grid_focus(&self) -> Option<&FocusHandle> {
        self.grid_focus.as_ref()
    }

    /// The dashboard's font overrides.
    #[must_use]
    pub fn font_overrides(&self) -> &crate::dashboard::FontOverrides {
        &self.fonts
    }

    /// Whether a popup of the window's holds the keyboard (the sidebar's row menu, which a right-click opens while
    /// the selection it narrowed is still on its way): until it closes the deck moves no focus, then it gives the
    /// shown terminal the keyboard the popup handed back, unless the popup's item gave it to `owner` (the sidebar's
    /// live rename field); an alert the item opened is a modal, which the deck waits on until it closes. Any other
    /// focus outside the terminals (a handle the popup restored to a field gone meanwhile) does not keep it.
    pub fn hold_keyboard(&mut self, held: bool, owner: Option<FocusHandle>) {
        // a popup that closes hands the keyboard back to the shown terminal even when nothing moved meanwhile: the
        // popup may have kept it (a dismissed menu, a dialog its item opened)
        self.held_move |= self.held && !held;
        self.held = held;
        self.owner = owner;
    }

    /// The page of session `session`.
    #[must_use]
    pub fn page(&self, session: Uuid) -> Option<&Page> {
        self.pages.get(&session)
    }

    /// The session whose page shows.
    #[must_use]
    pub fn shown(&self) -> Option<Uuid> {
        self.shown
    }

    /// What the deck's terminals share: the config, the launcher, the poster.
    #[must_use]
    pub fn env(&self) -> &Rc<DeckEnv> {
        &self.env
    }

    /// The colors the deck draws with.
    #[must_use]
    pub fn look(&self) -> Look {
        self.look
    }

    /// The shown page's focused terminal.
    #[must_use]
    pub fn shown_terminal(&self) -> Option<&PaneTerminal> {
        self.shown.and_then(|id| self.pages.get(&id)).and_then(Page::focus)
    }

    /// Every terminal of the deck.
    pub fn terminals(&self) -> impl Iterator<Item = &PaneTerminal> {
        self.pages.values().flat_map(|page| page.panes.iter())
    }

    /// The terminal of `session`'s pane carrying `token`.
    #[must_use]
    pub fn terminal(&self, session: Uuid, token: &str) -> Option<&PaneTerminal> {
        self.pages.get(&session)?.terminal(token)
    }

    /// What should hold the keyboard while the window has it: the shown page's terminal ask while it takes the
    /// keyboard, the overlay its layout focuses, else the shown terminal.
    #[must_use]
    pub fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        if self.grid.is_some() {
            return self.grid_focus.clone();
        }
        let page = self.shown.and_then(|id| self.pages.get(&id))?;
        page.keyboard(cx).map(|(_, handle)| handle)
    }

    /// Every overlay terminal's shared state, for the quit's teardown.
    pub fn overlay_terminals(&self) -> impl Iterator<Item = &Rc<Shared>> {
        self.pages
            .values()
            .flat_map(|page| page.overlays.terminals().map(overlay::OverlayTerminal::shared))
    }

    /// Where a GUI ask anchored to `target` is in the window as last laid out (upstream's `askAnchorFrame`); `None`
    /// while it is not on screen.
    #[must_use]
    pub fn anchor_bounds(&self, target: AnchorTarget) -> Option<Bounds<Pixels>> {
        let deck = self.bounds.get()?;
        let AnchorTarget::Page { session, slot } = target else {
            return Some(deck);
        };
        if self.shown != Some(session) {
            return None;
        }
        let rect = self.pages.get(&session)?.rect(slot)?;
        #[expect(clippy::cast_precision_loss, reason = "page coordinates are far below 2^24")]
        let (x, y, width, height) = (rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32);
        Some(Bounds {
            origin: deck.origin + point(px(x), px(y)),
            size: size(px(width), px(height)),
        })
    }

    /// Brings the deck in line with its window's sessions: pages for new sessions, gone sessions' pages removed, every
    /// page's panes synced with its session, the selected session's page shown and the keyboard given to what takes it
    /// there when that changed (its terminal ask, its focused overlay, else its focused pane). While the window is zoomed into one of its terminals, that session's page shows instead,
    /// selected or not, with that terminal alone (upstream's zoom layer). `modal`: the window's picker or GUI ask (or
    /// the palette) holds the keyboard.
    ///
    /// A pane the deck gives the keyboard is reported to the core at once (upstream's `becomeFirstResponder` fires for
    /// a programmatic focus as for a click, and an explicit refocus of the terminal that holds the keyboard changes no
    /// GPUI focus); a report queued behind later commands would hand the model a stale focus. Whether the core needs a
    /// settle for it.
    #[must_use = "a focus report the core took needs a settle"]
    pub fn reconcile(&mut self, core: &mut Core, modal: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mounted: HashSet<Uuid> = self.pages.keys().copied().collect();
        let plan = plan(core.library(), Some(self.window), &mounted);
        for id in &plan.remove {
            self.pages.remove(id);
            self.order.retain(|known| known != id);
        }
        for spec in &plan.add {
            self.add_page(core, spec, window, cx);
        }
        let window_opacity = crate::chrome::translucency(cx).effective(window.is_fullscreen());
        self.look = Look::of(&self.env.config(), core, window_opacity);
        let zoom = core
            .zoom_target(self.window)
            .filter(|zoom| self.pages.contains_key(&zoom.session));
        let grid = crate::dashboard::GridModel::of(core, self.window);
        self.zoomed = zoom.map(|zoom| zoom.session);
        // under the grid no page shows: it hosts its members' terminals itself
        let shown = zoom.map(|zoom| zoom.session).or(plan.shown).filter(|_| grid.is_none());
        let modal = modal || crate::quick::holds_keyboard(cx);
        let sync = PageSync {
            window: self.window,
            zoom,
            look: self.look,
            shown,
            focus_blocked: modal,
            ask_look: ask_dialog::Look::current(core, &self.env.config()),
        };
        let ids: Vec<Uuid> = self.order.clone();
        for id in ids {
            self.sync_page(core, &sync, id, window, cx);
        }
        if let Some(grid) = grid {
            self.show_grid(core, &grid, modal, window, cx);
            cx.notify();
            return false;
        }
        if self.grid.is_some() {
            self.leave_grid();
        }
        // a zoom keeps the keyboard on its terminal (upstream's `focusSplitPane` steps aside while zoomed)
        let refocus = zoom.is_none() && shown.is_some_and(|id| core.pane_focus_requested(id));
        let given = self.show(shown, refocus, modal, window, cx);
        cx.notify();
        given.is_some_and(|(session, token)| core.pane_focused(session, &token))
    }

    /// Shows the dashboard (the GTK build's `show_grid`): its member terminals drawn in their cells at the grid's font
    /// size, the rest hidden with their pages, and the grid's focus holding the keyboard unless a modal or a popup
    /// does.
    fn show_grid(
        &mut self,
        core: &Core,
        grid: &crate::dashboard::GridModel,
        modal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = core.library().store(self.window);
        let terminals: Vec<Option<(Uuid, String)>> = grid
            .members
            .iter()
            .zip(grid.tokens(store))
            .map(|(member, token)| token.map(|token| (member.session, token)))
            .collect();
        let mut members = Vec::new();
        for (session, token) in terminals.iter().flatten() {
            if let Some(pane) = self.terminal(*session, token) {
                pane.view.update(cx, |view, cx| view.set_shown(true, cx));
                members.push((token.clone(), Rc::clone(&pane.shared)));
            }
        }
        self.fonts.sync(&members, grid.font_size);
        self.grid = Some(Grid {
            cells: grid.cells(store, core.settings()),
            terminals,
        });
        self.shown = None;
        self.set_focused(None);
        let focus = self.grid_focus.get_or_insert_with(|| cx.focus_handle()).clone();
        // upstream's key catcher takes the keyboard back on every render, unless a dialog or a popup holds it
        if !modal && !self.held && !focus.is_focused(window) {
            window.focus(&focus, cx);
        }
    }

    /// The dashboard closed: the member terminals get their own font back and go back to their pages, and the next
    /// show gives the keyboard to the shown page's focused pane again.
    fn leave_grid(&mut self) {
        self.grid = None;
        self.fonts.sync(&[], None);
        self.set_focused(None);
    }

    /// The tokens the grid hosts on `session`'s page, which the page does not draw meanwhile.
    fn hosted(&self, session: Uuid) -> Vec<&str> {
        self.grid
            .iter()
            .flat_map(|grid| grid.terminals.iter().flatten())
            .filter(|(owner, _)| *owner == session)
            .map(|(_, token)| token.as_str())
            .collect()
    }

    /// The open dashboard over the deck.
    fn render_grid(&self, grid: &Grid, cx: &App) -> AnyElement {
        let cells = grid
            .cells
            .iter()
            .zip(&grid.terminals)
            .enumerate()
            .map(|(index, (chrome, terminal))| {
                let pane = terminal
                    .as_ref()
                    .and_then(|(session, token)| Some((self.pages.get(session)?, token)))
                    .and_then(|(page, token)| Some((page, page.terminal(token)?)));
                let lead = pane.and_then(|(page, pane)| {
                    let cover = page.cover(pane.token())?;
                    let slot = page.plan.as_ref()?.slot_of(pane.token()).unwrap_or(Slot::Primary);
                    let handle = pane.focus_handle(cx);
                    pane_lead::element(cover, slot, self.look.background, self.look.foreground, &handle)
                });
                crate::dashboard::CellParts {
                    index,
                    chrome: chrome.clone(),
                    terminal: pane.map(|(_, pane)| pane.view.clone().into_any_element()),
                    lead,
                }
            })
            .collect();
        let chrome = crate::chrome::current(cx);
        let look = crate::dashboard::Look {
            background: self.look.background,
            foreground: self.look.foreground,
            selection: chrome
                .selection_background
                .map(|fill| (fill, chrome.selection_foreground)),
        };
        let animate = crate::status_glyph::animations_enabled(cx);
        crate::dashboard::grid(self.window, cells, look, &self.env.poster, animate)
    }

    fn add_page(&mut self, core: &mut Core, spec: &PaneSpec, window: &mut Window, cx: &mut Context<Self>) {
        let mut page = Page::new(spec.session);
        let terminal = mount(&self.env, spec, window, cx);
        let surface = terminal.terminal.read(cx).surface();
        page.panes.push(terminal);
        let session = core
            .library_mut()
            .store_mut(spec.window)
            .and_then(|store| store.session_mut(spec.session));
        if let Some(session) = session {
            install(session, spec, Box::new(surface));
        }
        self.pages.insert(spec.session, page);
        self.order.push(spec.session);
    }

    /// Brings page `id` in line with its session: the terminals the model dropped go, the panes it lays out and has
    /// no terminal for are created and installed in their slots, and the layout follows the session and the zoom.
    fn sync_page(&mut self, core: &mut Core, sync: &PageSync, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let shown = sync.shown == Some(id);
        let covers: Vec<(String, PaneCover)> = self
            .pages
            .get(&id)
            .map(|page| {
                page.panes
                    .iter()
                    .filter(|pane| pane.spec.pane != StatusPane::Scratch)
                    .map(|pane| (pane.token().to_owned(), pane_lead::cover(core, pane.token())))
                    .collect()
            })
            .unwrap_or_default();
        let Some(store) = core.library_mut().store_mut(sync.window) else {
            return;
        };
        let Some(workspace) = store.workspace_for_session(id).map(Workspace::id) else {
            // held by a pending soft close: its terminals keep running, hidden, until the close is final or undone
            if let Some(page) = self.pages.get_mut(&id) {
                page.hide(cx);
                page.plan = None;
            }
            return;
        };
        let Some(session) = store.session_mut(id) else {
            return;
        };
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        page.retain(|token| session.pane_role_for_token(token).is_some());
        let needs = split::needs(session, sync.zoom);
        if needs.split
            && let Some(spec) = PaneSpec::split(sync.window, workspace, session)
        {
            mount_into(&self.env, page, session, &spec, window, cx);
        }
        if needs.scratch {
            let spec = PaneSpec::scratch(sync.window, workspace, session);
            mount_into(&self.env, page, session, &spec, window, cx);
        }
        // the first layout of a shown split seeds an even ratio, so a shown split always reports one (upstream's
        // `restoreDivider`); a zoomed window lays no split out
        if sync.zoom.is_none() && session.split.shown && session.split.ratio.is_none() {
            session.split.ratio = Some(SPLIT_RATIO_DEFAULT);
        }
        let layout = PanePlan::layout(session, sync.zoom);
        page.state.settle();
        let on_screen = shown.then_some(&layout);
        page.washes = Washes {
            pane: mute::muted(session, on_screen, sync.look.mute, sync.look.background),
            backdrop: mute::backdrop(session, on_screen, sync.look.background).map(|regions| (regions, sync.look.mute)),
        };
        let context = hud::HudContext {
            window: sync.window,
            workspace,
            env: &self.env,
        };
        hud::sync_page(&mut page.hud, session, &context, Some(&layout), shown, cx);
        let mut mount = overlay::Mount { hud: &context, window };
        overlay::sync_page(&mut page.overlays, session, &mut mount, &layout, shown, cx);
        page.plan = Some(layout);
        page.covers = covers;
        page.show(shown, cx);
        hold_keys(page, &self.env.poster);
        let opacity = crate::chrome::translucency(cx).opacity;
        sync_watermarks(&self.env, page, session, opacity, cx);
        sync_ask(page, session, sync, shown && sync.zoom.is_none(), &self.env.poster, cx);
    }

    /// The app's config changed (a reload): every terminal takes it, over its own overlay; with `keep_zoom` (an
    /// appearance flip) each keeps its live font size.
    pub fn apply_config(&self, keep_zoom: bool, cx: &mut Context<Self>) {
        for pane in self.terminals() {
            // a program's OSC 11 background survives the reload (upstream's `reapplySessionConfigIfNeeded`)
            let config = self.env.config_for(pane.effective_overlay().as_deref());
            pane.terminal.update(cx, |terminal, cx| {
                if keep_zoom {
                    terminal.flip_config(config, cx);
                } else {
                    terminal.reload_config(config, cx);
                }
            });
        }
        for panel in self.pages.values().filter_map(|page| page.hud.panel()) {
            panel.apply_config(&self.env, cx);
        }
        for overlay in self.pages.values().flat_map(|page| page.overlays.terminals()) {
            overlay.apply_config(&self.env, keep_zoom, cx);
        }
        cx.notify();
    }

    /// Shows the page of `shown` alone and gives its focused pane the keyboard when the page or that pane changed
    /// (upstream focuses the active session after a select or a create, and the focused pane within it), or when
    /// `refocus` (a `session.focus` asked for it, wherever focus sits now); with none shown the focus leaves the
    /// hidden terminals. While a modal holds the keyboard nothing is focused or unfocused, and the next show after it
    /// focuses again. The pane given the keyboard, as (session, pane token).
    fn show(
        &mut self,
        shown: Option<Uuid>,
        refocus: bool,
        modal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(Uuid, String)> {
        let changed = self.shown != shown;
        self.shown = shown;
        let Some(id) = shown else {
            self.set_focused(None);
            if changed && !modal && !self.held {
                window.blur(cx);
            }
            return None;
        };
        if modal {
            self.set_focused(None);
            return None;
        }
        let target = self.pages.get(&id).and_then(|page| page.keyboard(cx));
        let desired = target.as_ref().map(|(key, _)| (id, key.clone()));
        if self.held {
            self.held_move |= changed || refocus || desired != self.focused;
            return None;
        }
        if std::mem::take(&mut self.held_move) && self.keyboard_owned(window) {
            // the popup's item gave the keyboard to a field of its own: the move it waited for is moot
            self.set_focused(desired);
            return None;
        }
        let reporting = !changed && self.focus_moves.reporting.load(Ordering::Relaxed);
        if (desired == self.focused || reporting) && !refocus {
            return None;
        }
        if let Some((_, handle)) = target {
            window.focus(&handle, cx);
        }
        self.set_focused(desired.clone());
        // a dialog or an overlay the deck gave the keyboard is no pane focus the model hears of
        desired.filter(|(_, key)| !key.starts_with("ask:") && !overlay::is_overlay_key(key))
    }

    /// Whether a live keyboard owner outside the deck's terminals holds the window's keyboard: the owner the last
    /// hold named (an alert or a popover holding it is a modal, which the deck never moves the keyboard under).
    fn keyboard_owned(&self, window: &Window) -> bool {
        self.owner.as_ref().is_some_and(|owner| owner.is_focused(window))
    }

    /// Records where keyboard focus went, counting a move; a user move's report still queued is overtaken.
    fn set_focused(&mut self, focused: Option<(Uuid, String)>) {
        if self.focused != focused {
            self.focused = focused;
            self.focus_moves.count.fetch_add(1, Ordering::Relaxed);
            self.focus_moves.reporting.store(false, Ordering::Relaxed);
        }
    }

    /// Hands a granted pane its seed: it starts once laid out, or at once when it waits already. Whether this deck
    /// holds the pane.
    #[must_use = "a grant no deck holds is lost"]
    pub fn launch(&self, grant: &PaneSpawn) -> bool {
        let Some(pane) = self.terminal(grant.session, &grant.token) else {
            return false;
        };
        launch_seeded(&self.env, pane, &grant.seed);
        true
    }

    /// Puts a fresh terminal in place of the session pane `replacement` names (upstream's `replacePane`): the same
    /// token, page position and visibility, the replacement's command over the pane's own environment, and the keyboard
    /// when the replaced one had it. The replaced terminal goes with none of the pane's close paths: its exit is
    /// claimed first. False when this deck has no such pane.
    pub fn replace_pane(
        &mut self,
        core: &mut Core,
        replacement: &PaneReplacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (session, token) = (replacement.session, replacement.token.as_str());
        let Some(spec) = core
            .library()
            .store(self.window)
            .and_then(|store| PaneSpec::of_token(self.window, store, session, token))
        else {
            return false;
        };
        let Some(page) = self.pages.get_mut(&session) else {
            return false;
        };
        let Some(index) = page.panes.iter().position(|pane| pane.token() == token) else {
            return false;
        };
        let old = &page.panes[index];
        let had_focus = old.focus_handle(cx).is_focused(window);
        let old_cwd = old.spec.cwd.clone();
        let old_points = old.shared.font_points();
        let mut launch = spec.launch(&self.env.context);
        launch.command = Some(replacement.command.clone());
        launch.live.clone_from(&replacement.attach);
        // a remote replica's attach holds after its ssh, as the pane's first attach did
        launch.wait_after_command = replacement.remote;
        launch.env = merged_env(&launch.env, &replacement.env);
        launch.working_directory = Some(replacement.cwd.clone().unwrap_or(old_cwd).into());
        let spec = PaneSpec {
            font_size: Some(f64::from(old_points)),
            ..spec
        };
        let fresh = mount_with(&self.env, &spec, Some((launch, !replacement.remote)), window, cx);
        let surface = fresh.terminal.read(cx).surface();
        // the replaced terminal's events stop with its subscriptions: its exit is not the pane's any more
        let old = std::mem::replace(&mut page.panes[index], fresh);
        let installed = core
            .library_mut()
            .store_mut(self.window)
            .and_then(|store| store.session_mut(session))
            .and_then(|found| Some((found.pane_role_for_token(token)?, found)))
            .map(|(role, found)| found.pane_slot_mut(role).replace(Box::new(surface)));
        if let Some(Some(previous)) = installed {
            previous.teardown();
        }
        old.shared.teardown();
        if had_focus && let Some(fresh) = self.terminal(session, token) {
            let handle = fresh.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
        true
    }

    /// A terminal's event, routed to the model.
    fn terminal_event(
        &mut self,
        session: Uuid,
        token: &str,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.terminal(session, token) else {
            return;
        };
        if matches!(event, TerminalEvent::FocusFollowsMouse) {
            // as a click would: the focus listener reports it (Ghostty's `moveFocus`)
            let handle = pane.focus_handle(cx);
            window.focus(&handle, cx);
            return;
        }
        let points = pane.shared.font_points();
        if matches!(event, TerminalEvent::FontSize) && !pane.shared.take_font_report(points) {
            // the dashboard's transient size, or the revert from it: not the pane's own size
            return;
        }
        if let TerminalEvent::Pane(agterm_term::PaneEvent::Colors(colors)) = event {
            let opacity = crate::chrome::translucency(cx).opacity;
            osc_background_changed(&self.env, pane, colors, opacity, cx);
        }
        if let TerminalEvent::Pane(agterm_term::PaneEvent::Exited(status)) = event
            && status.held
        {
            // a held exit closes on the next key, as libghostty's prompt did
            pane.exit_held.set(true);
            refresh_hold(pane, &self.env.poster, session);
        }
        let focus = NotifyFocus {
            pane: pane.focus_handle(cx).is_focused(window),
            active: window.is_window_active(),
        };
        let ran = pane.ran.get();
        // the cover lasts until the takeover's sync, which runs after this event: the key asking for it is not reported
        let covered = pane.cover_holds.get();
        for report in pane::report(event, ran, &self.env.hostname, points, covered) {
            pane::post_report(&self.env.poster, session, token, report, focus);
        }
    }

    /// The user focused the terminal carrying `token`: the model's split focus follows, when it still shows and no
    /// later focus move overtook the report before it ran. A focus the deck gave itself was reported already.
    fn terminal_focused(&mut self, session: Uuid, token: &str, cx: &mut Context<Self>) {
        let shown = self
            .terminal(session, token)
            .is_some_and(|pane| pane.view.read(cx).is_shown());
        let focused = Some((session, token.to_owned()));
        if !shown || self.focused == focused {
            return;
        }
        self.set_focused(focused);
        let moves = Arc::clone(&self.focus_moves);
        moves.reporting.store(true, Ordering::Relaxed);
        let at = moves.count.load(Ordering::Relaxed);
        let token = token.to_owned();
        post(&self.env.poster, move |state: &mut AppState| {
            if moves.count.load(Ordering::Relaxed) == at {
                moves.reporting.store(false, Ordering::Relaxed);
                state.pane_focused(session, &token);
            }
        });
    }

    /// The user focused the overlay terminal keyed `key` of `session` (a click into it): the deck keeps it as its
    /// focus, and a pane overlay's (`pane_generation`) moves the split's focus to the pane it covers (upstream's pane
    /// overlay `onFocusChange`). A focus the deck gave itself was recorded already.
    pub fn overlay_focused(&mut self, session: Uuid, key: &str, pane_generation: Option<u64>, cx: &mut Context<Self>) {
        let shown = self
            .pages
            .get(&session)
            .is_some_and(|page| page.overlays.shows(key, cx));
        let focused = Some((session, key.to_owned()));
        if !shown || self.focused == focused {
            return;
        }
        self.set_focused(focused);
        let Some(generation) = pane_generation else {
            return;
        };
        post(&self.env.poster, move |state: &mut AppState| {
            if state.core.pane_overlay_focused(session, generation) {
                state.settle();
            }
        });
    }

    /// A press on the divider of `session`'s page: a double-click on an unmoved divider resets the even split, any
    /// other press starts a drag.
    fn divider_pressed(&mut self, session: Uuid, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.pages.get(&session) else {
            return;
        };
        if event.click_count >= 2 && page.state.is_reset(2) {
            post_ratio(&self.env.poster, session, SPLIT_RATIO_DEFAULT);
            return;
        }
        let ratio = page
            .plan
            .as_ref()
            .map_or(SPLIT_RATIO_DEFAULT, |plan| page.state.ratio(plan.ratio));
        page.state.begin_drag(ratio, event.position);
        window.refresh();
        cx.stop_propagation();
    }

    fn render_page(&self, page: &Page, visible: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = page.plan.as_ref() else {
            return div().into_any_element();
        };
        let session = page.session;
        let hosted = self.hosted(session);
        // a zoomed page shows its zoomed terminal alone (upstream's `terminal-zoom` layer)
        let zoomed = visible && self.zoomed == Some(session);
        let mut element = PageElement::new(plan.panes, plan.axis, plan.ratio, Rc::clone(&page.state));
        // the terminals, hidden ones under the page at full size; the grid draws the ones it hosts
        for pane in page
            .panes
            .iter()
            .filter(|pane| pane.spec.pane != StatusPane::Scratch && !hosted.contains(&pane.token()))
        {
            let shown = pane.view.read(cx).is_shown();
            element = element.child(
                terminal_wrapper(pane, shown, zoomed && shown),
                terminal_place(plan, pane.token()),
            );
        }
        for slot in [Slot::Primary, Slot::Split] {
            if let Some((color, opacity)) = page.washes.wash(slot, false) {
                element = element.child(wash(color, opacity), Place::Over(slot));
            }
        }
        for (token, cover) in &page.covers {
            let Some(slot) = plan
                .slot_of(token)
                .filter(|slot| plan.shows(*slot) && *slot != Slot::Scratch)
            else {
                continue;
            };
            let Some(terminal) = page.terminal(token) else { continue };
            let handle = terminal.focus_handle(cx);
            if let Some(cover) =
                pane_lead::element(cover, slot, self.look.background, self.look.foreground, &handle).filter(|_| visible)
            {
                element = element.child(cover, Place::Over(slot));
            }
        }
        if plan.shows_divider() {
            element = element
                .child(separator(self.look.foreground), Place::Separator)
                .child(Self::divider(session, plan.axis, cx), Place::Divider);
        }
        element = with_pane_overlays(element, page, plan, cx);
        for slot in [Slot::Primary, Slot::Split] {
            if let Some((color, opacity)) = page.washes.wash(slot, true) {
                element = element.child(wash(color, opacity), Place::Over(slot));
            }
        }
        if let Some(scratch) = page.panes.iter().find(|pane| pane.spec.pane == StatusPane::Scratch) {
            let shown = scratch.view.read(cx).is_shown();
            element = element.child(
                terminal_wrapper(scratch, shown, zoomed && shown),
                Place::Pane(Slot::Scratch),
            );
        }
        if let Some((regions, opacity)) = page.washes.backdrop.clone() {
            element = element.child(backdrop(regions, opacity, plan, Rc::clone(&page.state)), Place::Full);
        }
        element = with_program(element, page);
        if let (Some(panel), Some(placement)) = (hud::element(&page.hud), page.hud.placement()) {
            element = element.child(panel, Place::Hud(placement));
        }
        if let Some(ask) = page.ask.as_ref().filter(|_| visible) {
            let place = match ask.scope {
                AskScope::Session => Place::Full,
                AskScope::Pane(slot) => Place::Over(slot),
            };
            element = element.child(ask.dialog.clone(), place);
        }
        let poster = self.env.poster.clone();
        element = element.report_frames(Rc::new(move |frames| {
            post(&poster, move |state: &mut AppState| {
                state.report_hud_frames(session, &frames);
            });
        }));
        let poster = self.env.poster.clone();
        element
            .report_ratio(Rc::new(move |ratio| post_ratio(&poster, session, ratio)))
            .into_any_element()
    }

    /// The divider's grab band of `session`'s page.
    fn divider(session: Uuid, axis: agterm_core::model::SplitAxis, cx: &mut Context<Self>) -> impl IntoElement {
        let cursor = match axis {
            agterm_core::model::SplitAxis::LeftRight => CursorStyle::ResizeLeftRight,
            agterm_core::model::SplitAxis::TopBottom => CursorStyle::ResizeUpDown,
        };
        div()
            .id(SharedString::from(format!("split-divider-{session}")))
            .size_full()
            .cursor(cursor)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |deck, event: &MouseDownEvent, window, cx| {
                    deck.divider_pressed(session, event, window, cx);
                }),
            )
    }
}

impl Render for Deck {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // a translucent window's backing shows through: the terminals draw no background of their own then
        let see_through = crate::chrome::see_through(window, cx);
        let background = mute::color(self.look.background, if see_through { 0.0 } else { 1.0 });
        let pages: Vec<AnyElement> = self
            .order
            .iter()
            .filter_map(|id| self.pages.get(id).map(|page| (*id, page)))
            .map(|(id, page)| {
                let visible = Some(id) == self.shown;
                div()
                    .absolute()
                    .inset_0()
                    .when(!visible, Styled::invisible)
                    .child(self.render_page(page, visible, cx))
                    .into_any_element()
            })
            .collect();
        let recorded = Rc::clone(&self.bounds);
        let anchor = canvas(
            move |bounds, window, _| {
                if recorded.replace(Some(bounds)) != Some(bounds) {
                    // a GUI ask anchored here follows on the next frame
                    window.refresh();
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full();
        let grid = self.grid.as_ref().map(|grid| self.grid_layer(grid, cx));
        div()
            .id("deck")
            .debug_selector(|| "deck".to_owned())
            .relative()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(background)
            .child(anchor)
            .when(self.shown.is_none() && self.grid.is_none(), |deck| {
                deck.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(mute::color(self.look.foreground, 0.55))
                        .child(NO_SESSION),
                )
            })
            .children(pages)
            .children(grid)
    }
}

impl Deck {
    /// The grid's layer: the grid, and its focus holding the keyboard, which reads the grid's keys and swallows every
    /// other one so nothing reaches a terminal behind.
    fn grid_layer(&self, grid: &Grid, cx: &App) -> impl IntoElement {
        let (focus, poster, window) = (self.grid_focus.clone(), self.env.poster.clone(), self.window);
        div()
            .absolute()
            .inset_0()
            .when_some(focus, |layer, focus| layer.track_focus(&focus))
            .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
                if let Some(key) = crate::dashboard::grid_key(&event.keystroke) {
                    post(&poster, move |state: &mut AppState| state.dashboard_key(window, key));
                }
                cx.stop_propagation();
            })
            .child(self.render_grid(grid, cx))
    }
}

/// `element` with `page`'s pane overlays over the panes `plan` puts them on (hidden ones at the full page, undrawn).
fn with_pane_overlays(mut element: PageElement, page: &Page, plan: &PanePlan, cx: &App) -> PageElement {
    for pane_overlay in page.overlays.panes() {
        let place = overlay::slot_of(plan, pane_overlay.generation()).map_or(Place::Full, Place::Over);
        element = element.child(overlay::pane_element(pane_overlay, cx), place);
    }
    for pane_page in page.overlays.pane_pages() {
        let place = overlay::slot_of(plan, pane_page.generation()).map_or(Place::Full, Place::Over);
        element = element.child(overlay::pane_page_element(pane_page), place);
    }
    element
}

/// `element` with `page`'s program panel, where it shows.
fn with_program(element: PageElement, page: &Page) -> PageElement {
    let Some(program) = overlay::program_element(&page.overlays) else {
        return element;
    };
    let placement = page
        .overlays
        .placement()
        .unwrap_or(overlay::ProgramPlacement { fraction: None });
    element.child(program, Place::Program(placement))
}

/// Where a page lays the terminal carrying `token` out: at its slot while the plan draws it, else at the full page,
/// hidden under the rest.
fn terminal_place(plan: &PanePlan, token: &str) -> Place {
    match plan.slot_of(token) {
        Some(slot) if plan.draws(token) => Place::Pane(slot),
        _ => Place::Full,
    }
}

/// A pane's terminal view inside a wrapper that is drawn only while the view shows. A key press on a terminal
/// holding its keys (a lead cover, a held exit) runs its hold in the terminal's key path (`Shared::held_key`).
fn terminal_wrapper(pane: &PaneTerminal, shown: bool, zoomed: bool) -> AnyElement {
    let wrapper = div()
        .size_full()
        .when(!shown, Styled::invisible)
        .child(pane.view.clone());
    if zoomed {
        // the zoomed terminal: upstream's accessibility identifier as its element id
        return wrapper
            .id("terminal-zoom")
            .debug_selector(|| "terminal-zoom".to_owned())
            .accessibility_id("terminal-zoom")
            .role(gpui_kit::Role::Group)
            .into_any_element();
    }
    wrapper.into_any_element()
}

/// A solid wash at `opacity` over the area it is laid out on; it takes nothing.
fn wash(rgb: Rgb, opacity: f64) -> impl IntoElement {
    div().size_full().bg(mute::color(rgb, opacity))
}

/// The separator line between the split's panes, in the terminal's foreground at the chrome's separator alpha.
fn separator(foreground: Rgb) -> impl IntoElement {
    div().size_full().bg(mute::color(
        foreground,
        agterm_core::chrome::ChromeColors::SEPARATOR_ALPHA,
    ))
}

/// A floating program's backdrop: its regions flattened over the page as laid out at paint time.
fn backdrop(regions: mute::Regions, opacity: f64, plan: &PanePlan, state: Rc<PageState>) -> impl IntoElement {
    let plan = plan.clone();
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let (width, height) = state.size.get();
            let geometry = split::geometry(plan.panes, plan.axis, state.ratio(plan.ratio), width, height);
            let flat = mute::flatten(&regions, plan.panes, &geometry, width, height);
            mute::paint_backdrop(window, bounds, &flat, opacity);
        },
    )
    .size_full()
}

/// Brings `page`'s terminal ask in line with `session`'s: a dialog for the ask while it shows on the page (a new one
/// per ask, as upstream's view is keyed by its id), taken down otherwise. `shown`: the page shows its session with
/// nothing over it (selected, no zoom).
fn sync_ask(page: &mut Page, session: &Session, sync: &PageSync, shown: bool, poster: &Poster, cx: &mut Context<Deck>) {
    let scope = ask_dialog::shown_scope(session, shown);
    let pending = session.ask().filter(|_| scope.is_some()).map(|ask| &ask.ask);
    let (Some(scope), Some(pending)) = (scope, pending) else {
        page.ask = None;
        return;
    };
    let current = page
        .ask
        .as_ref()
        .map(|ask| ask.dialog.clone())
        .filter(|dialog| dialog.read(cx).id() == pending.id);
    let look = sync.ask_look.clone();
    let dialog = if let Some(dialog) = current {
        dialog.update(cx, |dialog, cx| dialog.set_look(look, cx));
        dialog
    } else {
        let (owner, poster) = (AskOwner::Session(session.id()), poster.clone());
        cx.new(|cx| AskDialog::new(pending, owner, look, poster, cx))
    };
    page.ask = Some(PageAsk {
        dialog,
        scope,
        wants_focus: ask_dialog::wants_focus(session, sync.focus_blocked),
    });
}

/// Mounts `spec`'s terminal in `page` and installs it in `session`'s slot.
fn mount_into(
    env: &Rc<DeckEnv>,
    page: &mut Page,
    session: &mut Session,
    spec: &PaneSpec,
    window: &mut Window,
    cx: &mut Context<Deck>,
) {
    let terminal = mount(env, spec, window, cx);
    let surface = terminal.terminal.read(cx).surface();
    page.panes.push(terminal);
    install(session, spec, Box::new(surface));
}

/// Puts `surface` in the slot of `spec`'s pane; a terminal already there is torn down (it never should be).
fn install(session: &mut Session, spec: &PaneSpec, surface: Box<dyn agterm_host::Surface>) {
    if let Some(previous) = session.pane_slot_mut(spec.pane).replace(surface) {
        tracing::error!(session = %spec.session, pane = spec.pane.as_str(), "a pane got a second terminal");
        previous.teardown();
    }
}

/// A terminal for `spec`: a session pane's waits for its seed (it asks once laid out), the scratch starts from the
/// spec.
fn mount(env: &Rc<DeckEnv>, spec: &PaneSpec, window: &mut Window, cx: &mut Context<Deck>) -> PaneTerminal {
    let launch = (!spec.gated()).then(|| (spec.launch(&env.context), false));
    mount_with(env, spec, launch, window, cx)
}

/// A terminal for `spec` starting with `launch` (and whether it attaches to a live daemon), or gated without one.
fn mount_with(
    env: &Rc<DeckEnv>,
    spec: &PaneSpec,
    launch: Option<(agterm_term::Launch, bool)>,
    window: &mut Window,
    cx: &mut Context<Deck>,
) -> PaneTerminal {
    let config = (*env.config()).clone();
    let ran: RanCell = Rc::new(Cell::new(Ran::default()));
    let terminal_spec = crate::terminal::TerminalSpec {
        live: launch.as_ref().is_some_and(|(_, live)| *live),
        ..spec.terminal_spec()
    };
    let terminal = if let Some((launch, _)) = launch {
        ran.set(Ran::of(&launch));
        let spawn = (env.launcher)(&launch);
        cx.new(|cx| Terminal::new(terminal_spec, config, spawn, cx))
    } else {
        let (poster, session, token) = (env.poster.clone(), spec.session, spec.token.clone());
        let ask = Box::new(move || {
            post(&poster, move |state: &mut AppState| {
                state.pane_spawn_ready(session, &token);
            });
        });
        cx.new(|cx| Terminal::gated(terminal_spec, config, ask, cx))
    };
    let shared = Rc::clone(terminal.read(cx).shared());
    shared.set_owner(Some(SurfaceOwner::Pane {
        session: spec.session,
        token: spec.token.clone(),
    }));
    let view = cx.new(|cx| {
        let mut view = TerminalView::new(&terminal, ViewRole::Owner, cx);
        view.set_shown(false, cx);
        view
    });
    let (session, token) = (spec.session, spec.token.clone());
    let events = cx.subscribe_in(&terminal, window, move |deck, _, event: &TerminalEvent, window, cx| {
        deck.terminal_event(session, &token, event, window, cx);
    });
    let handle = view.read(cx).focus_handle(cx);
    let token = spec.token.clone();
    let focus = cx.on_focus(&handle, window, move |deck, _, cx| {
        deck.terminal_focused(session, &token, cx);
    });
    PaneTerminal {
        spec: spec.clone(),
        terminal,
        view,
        shared,
        ran,
        cover_holds: Rc::new(Cell::new(false)),
        exit_held: Rc::new(Cell::new(false)),
        overlay: RefCell::new(None),
        osc_background: RefCell::new(None),
        _subscriptions: vec![events, focus],
    }
}

/// Hands `pane` its seed's launch.
fn launch_seeded(env: &DeckEnv, pane: &PaneTerminal, seed: &LaunchSeed) {
    let launch = pane.spec.seeded(seed, &env.context);
    pane.ran.set(Ran::of(&launch));
    let spawn = (env.launcher)(&launch);
    pane.shared.launch_with(spawn, seed.live);
}

/// Applies `session`'s background (and its per-pane overrides) to the terminals of its page, on every page sync, each
/// only when its own changed ([`crate::watermark`]), a solid color at the window's `opacity`; a program's OSC 11
/// background follows a change of the opacity too (upstream's `reapplyColorBackgroundIfNeeded`), and a background
/// applied over it takes its place (upstream's `applyWatermarkFromSession` releases the OSC latch).
fn sync_watermarks(env: &DeckEnv, page: &Page, session: &Session, opacity: f64, cx: &mut App) {
    let foreground = env.config().theme.foreground;
    let foreground = Rgb::new(foreground.r, foreground.g, foreground.b);
    let state_dir = env.watermarks.state_dir.as_path();
    for pane in &page.panes {
        let Some(role) = session.pane_role_for_token(pane.token()) else {
            continue;
        };
        let desired = watermark::desired(session, role, foreground, state_dir, opacity);
        let Some(stale) = watermark::change(&pane.shared, &desired) else {
            let lifted = pane.osc_background.borrow().as_ref().map(|(_, lifted)| *lifted);
            if lifted.is_some_and(|lifted| (lifted - opacity).abs() > f64::EPSILON) {
                if let Some((_, lifted)) = &mut *pane.osc_background.borrow_mut() {
                    *lifted = opacity;
                }
                let config = env.config_for(pane.effective_overlay().as_deref());
                pane.terminal.update(cx, |terminal, cx| terminal.set_config(config, cx));
            }
            continue;
        };
        let resolved = watermark::materialize(&desired, state_dir);
        let overlay = Some(watermark::overlay(&desired, resolved.as_deref())).filter(|overlay| !overlay.is_empty());
        let config = env.config_for(overlay.as_deref());
        *pane.overlay.borrow_mut() = overlay;
        *pane.osc_background.borrow_mut() = None;
        pane.terminal.update(cx, |terminal, cx| terminal.set_config(config, cx));
        watermark::remember(&pane.shared, desired, stale);
    }
}

/// A program changed `pane`'s colors: its background goes through upstream's `OSCBackgroundPolicy` against the color
/// the terminal's config seeded (its own color background, else the theme's). A set lifts the terminal's
/// `background-opacity` to the window's `opacity` so the color shows over a translucent window; a reset (OSC 111)
/// drops the lift, back to the terminal's own overlay.
fn osc_background_changed(
    env: &DeckEnv,
    pane: &PaneTerminal,
    colors: &agterm_term::TerminalColors,
    opacity: f64,
    cx: &mut App,
) {
    use agterm_core::osc_background::{self, Decision};
    let Some(background) = colors.background else {
        return;
    };
    let hex = |color: agterm_term::Rgb| format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);
    let incoming = hex(background);
    let theme = hex(env.config().theme.background);
    let own = watermark::own_color(&pane.shared);
    let current = pane.osc_background.borrow().as_ref().map(|(color, _)| color.clone());
    let baseline = osc_background::baseline(current.is_some(), own.as_deref(), Some(&theme));
    let decision = osc_background::decide(&incoming, baseline, current.as_deref());
    match decision {
        Decision::Apply(color) => *pane.osc_background.borrow_mut() = Some((color, opacity)),
        Decision::Reset => *pane.osc_background.borrow_mut() = None,
        Decision::Ignore => return,
    }
    let config = env.config_for(pane.effective_overlay().as_deref());
    pane.terminal.update(cx, |terminal, cx| terminal.set_config(config, cx));
}

/// Marks every terminal of `page` holding its keys or not: covered or waiting to be attached again (a key press asks
/// the core to take the pane over or retry), or its program's exit held (a key press closes the pane).
fn hold_keys(page: &Page, poster: &Poster) {
    for pane in &page.panes {
        let holds = page.cover(pane.token()).is_some_and(|cover| cover.holds_keys);
        pane.cover_holds.set(holds);
        refresh_hold(pane, poster, page.session);
    }
}

/// Sets `pane`'s key hold from its exit and its cover.
fn refresh_hold(pane: &PaneTerminal, poster: &Poster, session: Uuid) {
    let token = pane.token().to_owned();
    let poster = poster.clone();
    let hold: Option<KeyHold> = if pane.exit_held.get() {
        let held = Rc::clone(&pane.exit_held);
        Some(Rc::new(move || {
            if held.replace(false) {
                let token = token.clone();
                post(&poster, move |state: &mut AppState| state.pane_exited(session, &token));
            }
        }))
    } else if pane.cover_holds.get() {
        Some(Rc::new(move || {
            let token = token.clone();
            post(&poster, move |state: &mut AppState| {
                if state.core.take_pane_lead(session, &token) {
                    state.settle();
                }
            });
        }))
    } else {
        None
    };
    pane.shared.hold_keys(hold);
}

fn post_ratio(poster: &Poster, session: Uuid, ratio: f64) {
    post(poster, move |state: &mut AppState| {
        state.set_split_ratio(session, ratio);
    });
}

/// `base` with every variable of `overlay` set over it: a replaced value keeps its place, a new one is appended.
#[must_use]
pub fn merged_env(base: &[(String, String)], overlay: &[(String, String)]) -> Vec<(String, String)> {
    let mut merged = base.to_vec();
    for (key, value) in overlay {
        match merged.iter_mut().find(|(known, _)| known == key) {
            Some(entry) => entry.1.clone_from(value),
            None => merged.push((key.clone(), value.clone())),
        }
    }
    merged
}

#[cfg(test)]
mod tests;
