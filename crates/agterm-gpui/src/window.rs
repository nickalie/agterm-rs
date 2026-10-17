//! One window over one model window's store (upstream's `WindowContentView.swift`; ported from the GTK build's
//! `window.rs`): the deck under the window's chrome, titled after the active session, reporting its on-screen state for
//! `window.list`, its activation as frontmost (upstream's `WindowAccessor.reportFrontmost`) and its close (upstream's
//! window `willClose`), and applying the window commands' requests. [`crate::windows`] owns the set of them.
//!
//! The window's content is a column: the title bar (`titlebar.rs`), then the sidebar's place (Task 19) beside the
//! deck, all on the chrome's background (`chrome.rs`); a window the client decorates gets resize handles along its
//! free edges. Over it sit the window's modal layers ([`Layer`]), in the GTK build's order: the scrollback search bar, the
//! Settings popup, the GUI ask, the native picker, the command palette, the Ctrl-Tab switcher and the title bar's
//! popovers; each attaches its view to its layer ([`WindowView::set_layer`]), drawn over the whole window while
//! attached. The window's alerts ([`crate::modal`]) come above everything.
//!
//! The Settings popup ([`crate::settings`]) comes up over the window Settings… was asked from: centered over a dimmed
//! layer that takes every click, a click outside the panel closing it as Escape and its close button do. It takes the
//! keyboard when it opens and keeps it as a modal does (a GUI ask, a picker or the palette arriving over it takes it,
//! and gives it back to it once it goes; so does an alert, and so does a control inside it that held the keyboard and
//! is no longer drawn); once it closes the shown terminal has the keyboard back.
//!
//! The GUI ask ([`crate::ask_dialog`]) is the window's own: one comes up with the model's modal slot, takes the
//! keyboard, and is drawn over its anchor while the rest of the window takes no clicks. So is the native picker
//! ([`crate::picker`], the slot's other tenant) and, while this window draws it, the command palette
//! ([`crate::palette`]): a new one takes the keyboard. While a GUI ask, a picker, the palette, an alert or a popover
//! holds the keyboard the deck moves none; once it closes the window's sync gives the keyboard back to the shown
//! terminal (an alert or a popover closing posts that sync), and an alert closing over a picker, a palette or a GUI ask
//! gives it back to that. The scrollback search bar ([`crate::search`]) sits at the terminal area's top right and takes
//! the keyboard when it comes up unless a modal holds it; the search's end gives it back to the terminal. The Ctrl-Tab
//! switcher ([`crate::session_switcher`]) never takes it.
//!
//! The window's own keys come first ([`intercept_key`], which the key monitor runs ahead of the keymap, any field's
//! bindings and the terminal): Ctrl+Tab and a walk's Escape are the switcher's, and the picker's, the palette's and
//! the search bar's fields hand their navigation keys to their panel (upstream's `onKeyPress` handlers, the GTK
//! build's capture-phase controllers). Control going up ends a walk (the root's modifier listener).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use agterm_core::model::{AppStore, Session, WindowLibrary};
use agterm_core::titlebar::UNTITLED;
use agterm_core::{Core, WindowRequest, WindowSize, WindowState};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, DismissEvent, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyUpEvent, ModifiersChangedEvent, MouseButton, ParentElement as _, Render,
    Styled as _, Subscription, Window, div, px, size,
};
use uuid::Uuid;

use crate::ask_dialog::{AskDialog, AskOwner};
use crate::deck::{AnchorTarget, Deck};
use crate::keymap_dispatch::KeyOrigin;
use crate::modal::Metrics;
use crate::palette::{Palette, PaletteView};
use crate::picker::Picker;
use crate::popovers::Popovers;
use crate::search::SearchBar;
use crate::session_switcher::SessionSwitcher;
use crate::settings::{Opening, SettingsView};
use crate::sidebar::Sidebar;
use crate::state::{AppState, FirstFocus, Poster, post};
use crate::terminal::input::KeyStroke;
use crate::titlebar::{Cover, Titlebar, TitlebarEvent};

/// The window title: the active session's display name, then the window's custom name when it has one (upstream's
/// `WindowTitleSync.title`).
#[must_use]
pub fn window_title(library: &WindowLibrary, window: Option<Uuid>) -> String {
    let session = window
        .and_then(|id| library.store(id))
        .and_then(AppStore::active_session)
        .map_or_else(|| UNTITLED.to_owned(), Session::display_name);
    match window.and_then(|id| library.custom_window_name(id)) {
        Some(name) => format!("{session} — {name}"),
        None => session,
    }
}

/// The title of an open dashboard: "Dashboard", then the window's custom name when it has one (upstream's
/// `dashboardWindowTitle`).
#[must_use]
pub fn dashboard_title(library: &WindowLibrary, window: Uuid) -> String {
    match library.custom_window_name(window) {
        Some(name) => format!("Dashboard — {name}"),
        None => "Dashboard".to_owned(),
    }
}

/// Where the terminal area starts in window `window`'s content: past the sidebar and its divider while the sidebar
/// shows, else at the edge (upstream's `terminalAreaInset`; a zoom or the dashboard covers the sidebar).
#[must_use]
pub fn terminal_area_inset(core: &Core, window: Uuid) -> f64 {
    if core.zoom_target(window).is_some() || core.dashboard(window).is_some() {
        return 0.0;
    }
    core.library()
        .store(window)
        .map(AppStore::sidebar)
        .filter(|sidebar| sidebar.visible)
        .map_or(0.0, |sidebar| sidebar.width + 1.0)
}

/// A modal layer over the window's content, back to front (the GTK build's `modal_host` overlays).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Search,
    Settings,
    Ask,
    Picker,
    Palette,
    Switcher,
    Popover,
}

/// The window's GUI ask: its dialog and what it covers (`None`: nowhere on screen; the panel waits).
struct WindowAsk {
    dialog: Entity<AskDialog>,
    target: Option<AnchorTarget>,
}

/// Full screen and maximized as last painted: `window.list` reports a state once a frame in it is drawn, so a caller
/// that waits for the new state before toggling back is not dropped by a compositor that ignores a request arriving
/// before the client committed the state it configured (the GTK build's `Painted`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Painted {
    pub fullscreen: bool,
    pub maximized: bool,
}

impl Painted {
    fn of(window: &Window) -> Self {
        Self {
            fullscreen: window.is_fullscreen(),
            maximized: window.is_maximized(),
        }
    }

    /// The model's state for this, on a display of `display`, `minimized` as [`reported_minimized`] has it (`None`
    /// where the display reads no minimized state back).
    #[must_use]
    pub fn state(self, display: Option<WindowSize>, minimized: Option<bool>) -> WindowState {
        WindowState {
            fullscreen: self.fullscreen,
            // maximized, not full screen (`WindowState::zoomed`)
            zoomed: self.maximized && !self.fullscreen,
            minimized,
            display,
        }
    }
}

/// How long a minimize or restore asked of an X11 window is reported as done before the window manager confirms it
/// (upstream's settle poll budget, 30 × 0.05 s, the GTK build's `MINIMIZE_SETTLE`); after it the state read back is
/// reported again.
pub const MINIMIZE_SETTLE: Duration = Duration::from_millis(1500);

/// The minimized state to report, given the one `read` back and the one a request is `pending` on, and whether the
/// request is still pending (the GTK build's `reported_minimized`). The window manager confirms an iconify a round
/// trip after it is asked, so until the read back catches up the requested state is reported: a quick `off` or
/// `toggle` then acts on the window's new state.
#[must_use]
pub fn reported_minimized(read: Option<bool>, pending: Option<bool>) -> (Option<bool>, bool) {
    match pending {
        Some(desired) if read.is_some() && read != Some(desired) => (Some(desired), true),
        _ => (read, false),
    }
}

/// `window`'s minimized state as the window manager reports it: X11's `_NET_WM_STATE_HIDDEN` (GPUI patch
/// `gpui-pre-linux` 0003); `None` on Wayland, whose xdg-shell has no such state for a client, and elsewhere.
#[must_use]
pub fn read_minimized(window: &Window) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        gpui_linux::x11_window_minimized(x11_id(window)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        None
    }
}

/// The size of the display `window` is on, in logical pixels.
fn display_size(window: &Window, cx: &App) -> Option<WindowSize> {
    let bounds = window.display(cx)?.bounds();
    let side = |value: gpui_kit::Pixels| {
        let value = f32::from(value).round();
        #[expect(clippy::cast_possible_truncation, reason = "a display side fits an i64")]
        let value = value as i64;
        value
    };
    Some(WindowSize {
        width: side(bounds.size.width),
        height: side(bounds.size.height),
    })
}

/// The window's root view.
pub struct WindowView {
    id: Uuid,
    deck: Entity<Deck>,
    titlebar: Entity<Titlebar>,
    sidebar: Entity<Sidebar>,
    popovers: Entity<Popovers>,
    ask: Option<WindowAsk>,
    /// The native picker while the modal slot holds a pick.
    picker: Option<Entity<Picker>>,
    /// The command palette while this window draws it.
    palette: Option<Entity<Palette>>,
    /// The Settings popup while it is up, with the subscriptions that take it down when it asks and give it the
    /// keyboard back when what held it went away.
    settings: Option<(Entity<SettingsView>, [Subscription; 2])>,
    search: Entity<SearchBar>,
    switcher: Entity<SessionSwitcher>,
    /// Whether an alert was up at the last sync: the GUI ask it covered takes the keyboard back once it goes.
    alert_up: bool,
    layers: Vec<(Layer, AnyView)>,
    title: String,
    /// Set once the model closed the window, so its close request goes through.
    closing: Rc<Cell<bool>>,
    /// The user's close is being confirmed: a second close meanwhile asks nothing (upstream's `sheetOpen`).
    close_asked: Rc<Cell<bool>>,
    /// The platform background last asked for (opaque, transparent or blurred: [`crate::chrome::Translucency`]).
    appearance: Cell<gpui_kit::WindowBackgroundAppearance>,
    painted: Rc<Cell<Painted>>,
    /// The minimized state last asked of an X11 window and the request's number, until the window manager confirms it
    /// or [`MINIMIZE_SETTLE`] passes ([`reported_minimized`]).
    pending_minimized: Rc<Cell<Option<(bool, u64)>>>,
    /// Numbers the minimize requests, so only the latest one's settle clears the pending state.
    minimize_requests: Cell<u64>,
    /// What the window's first activation knows of its build, until that activation or [`FIRST_FOCUS_GRACE`] after
    /// its first frame.
    first_focus: Rc<Cell<Option<FirstFocus>>>,
    poster: Poster,
    _subscriptions: Vec<Subscription>,
}

impl WindowView {
    /// The root of model window `id`, over `deck`, built when the core's frontmost takes stood at `frontmost_takes`;
    /// its handlers post through `poster`.
    pub fn new(
        id: Uuid,
        deck: Entity<Deck>,
        poster: Poster,
        frontmost_takes: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let closing = Rc::new(Cell::new(false));
        let (posting, closed) = (poster.clone(), Rc::clone(&closing));
        window.on_window_should_close(cx, move |_, _| {
            if closed.get() || posting.upgrade().is_none() {
                return true;
            }
            post(&posting, move |state: &mut AppState| state.close_requested(id));
            false
        });
        crate::keymap_dispatch::register_window(window.window_handle(), KeyOrigin::Window(id));
        let posting = poster.clone();
        let first_focus = Rc::new(Cell::new(Some(FirstFocus::Built { frontmost_takes })));
        let (fresh, expiring) = (Rc::clone(&first_focus), Rc::clone(&first_focus));
        window.on_next_frame(move |_, cx| {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FIRST_FOCUS_GRACE).await;
                expiring.set(None);
            })
            .detach();
        });
        let activation = cx.observe_window_activation(window, move |view, window, cx| {
            if !window.is_window_active() {
                // a walk ends when the window loses the keyboard
                view.switcher.update(cx, SessionSwitcher::reset);
                check_keyboard_left(cx);
            }
            if window.is_window_active() {
                let first = fresh.take();
                post(&posting, move |state: &mut AppState| state.window_activated(id, first));
            } else {
                post(&posting, move |state: &mut AppState| state.key_window_lost(id));
            }
        });
        let bounds = cx.observe_window_bounds(window, |_, _, cx| cx.notify());
        let titlebar = cx.new(|_| Titlebar::new(id, poster.clone()));
        let sidebar = cx.new(|_| Sidebar::new(id, poster.clone()));
        let popovers = cx.new(|cx| Popovers::new(id, poster.clone(), cx));
        let opening = cx.subscribe_in(&titlebar, window, |view, _, event: &TitlebarEvent, window, cx| {
            let TitlebarEvent::Popover { kind, anchor } = *event;
            view.popovers
                .update(cx, |popovers, cx| popovers.toggle(kind, anchor, window, cx));
            view.sync_popover_layer(cx);
        });
        let popped = cx.observe(&popovers, |view, _, cx| view.sync_popover_layer(cx));
        // an alert opening or closing (anywhere: the registry is the app's) is the window's to re-sync, so the
        // keyboard goes back to the shown terminal once the last one is answered
        let posting = poster.clone();
        let alerts = cx.observe_global::<crate::modal::Alerts>(move |_, _| post(&posting, |_| {}));
        let search = cx.new(|cx| SearchBar::new(id, poster.clone(), window, cx));
        let switcher = cx.new(|_| SessionSwitcher::new(id, poster.clone()));
        // both draw nothing while hidden, and neither takes a click: attached for good
        let layers = vec![
            (Layer::Search, AnyView::from(search.clone())),
            (Layer::Switcher, AnyView::from(switcher.clone())),
        ];
        Self {
            id,
            deck,
            titlebar,
            sidebar,
            popovers,
            ask: None,
            picker: None,
            palette: None,
            settings: None,
            search,
            switcher,
            alert_up: false,
            layers,
            title: UNTITLED.to_owned(),
            closing,
            close_asked: Rc::new(Cell::new(false)),
            appearance: Cell::new(gpui_kit::WindowBackgroundAppearance::Opaque),
            painted: Rc::new(Cell::new(Painted::default())),
            pending_minimized: Rc::new(Cell::new(None)),
            minimize_requests: Cell::new(0),
            first_focus,
            poster,
            _subscriptions: vec![activation, bounds, opening, popped, alerts],
        }
    }

    /// The window's popovers.
    #[must_use]
    pub fn popovers(&self) -> &Entity<Popovers> {
        &self.popovers
    }

    /// The window's GUI ask dialog while one is up.
    #[must_use]
    pub fn ask_dialog(&self) -> Option<&Entity<AskDialog>> {
        self.ask.as_ref().map(|ask| &ask.dialog)
    }

    /// Attaches the popovers while one is open; a popover that closed posts the sync that hands the keyboard back.
    fn sync_popover_layer(&mut self, cx: &mut Context<Self>) {
        let open = self.popovers.read(cx).is_open();
        if open == self.has_layer(Layer::Popover) {
            return;
        }
        let view = open.then(|| AnyView::from(self.popovers.clone()));
        self.set_layer(Layer::Popover, view, cx);
        if !open {
            post(&self.poster, |_| {});
        }
    }

    /// Brings the GUI ask in line with the model's modal slot: a dialog per ask (a new one takes the keyboard, as
    /// does the one shown when `refocus`, an alert over it having closed), what it covers, and none once it ends.
    fn sync_ask(&mut self, core: &Core, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let pending = core
            .modal_slot(id)
            .and_then(agterm_core::modal::PickController::pending_ask);
        let Some(pending) = pending else {
            if self.ask.take().is_some() {
                self.set_layer(Layer::Ask, None, cx);
            }
            return;
        };
        let target = crate::deck::anchor_target(core, id, pending);
        let look = crate::ask_dialog::Look::current(core, &self.deck.read(cx).env().config());
        if let Some(ask) = self.ask.as_mut().filter(|ask| ask.dialog.read(cx).id() == pending.id) {
            ask.target = target;
            ask.dialog.update(cx, |dialog, cx| dialog.set_look(look, cx));
            if refocus {
                let focus = ask.dialog.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            return;
        }
        let poster = self.poster.clone();
        let dialog = cx.new(|cx| AskDialog::new(pending, AskOwner::Window(id), look, poster, cx));
        let focus = dialog.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.set_layer(Layer::Ask, Some(dialog.clone().into()), cx);
        self.ask = Some(WindowAsk { dialog, target });
    }

    /// Brings the native picker in line with the model's modal slot: a picker per pick (a new one takes the keyboard,
    /// as does the one shown when `refocus`, an alert over it having closed), placed over the terminal area, and none
    /// once the slot empties.
    fn sync_picker(
        &mut self,
        core: &Core,
        place: (f64, Metrics),
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.id;
        let pick = core
            .modal_slot(id)
            .and_then(agterm_core::modal::PickController::pending);
        let Some(pick) = pick else {
            if self.picker.take().is_some() {
                self.set_layer(Layer::Picker, None, cx);
            }
            return;
        };
        let (inset, metrics) = place;
        if let Some(picker) = self.picker.as_ref().filter(|picker| picker.read(cx).id() == pick.id) {
            picker.update(cx, |picker, cx| picker.place(inset, metrics, cx));
            if refocus {
                let focus = picker.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            return;
        }
        let (poster, pick_id) = (self.poster.clone(), pick.id.clone());
        let resolve: crate::picker::Resolve = Rc::new(move |result| {
            let pick_id = pick_id.clone();
            post(&poster, move |state: &mut AppState| {
                state.resolve_pick(id, &pick_id, result);
            });
        });
        let picker = cx.new(|cx| Picker::new(pick, inset, metrics, resolve, window, cx));
        let focus = picker.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.set_layer(Layer::Picker, Some(picker.clone().into()), cx);
        self.picker = Some(picker);
    }

    /// Brings the command palette in line with the core: a palette per generation this window draws (a new one takes
    /// the keyboard, as does the one shown when `refocus`), refreshed while unchanged, and none once it closes.
    fn sync_palette(
        &mut self,
        core: &Core,
        place: (f64, Metrics),
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = PaletteView::of(core, self.id) else {
            if self.palette.take().is_some() {
                self.set_layer(Layer::Palette, None, cx);
            }
            return;
        };
        let (inset, metrics) = place;
        let generation = view.generation;
        if let Some(palette) = self
            .palette
            .as_ref()
            .filter(|palette| palette.read(cx).generation() == generation)
        {
            palette.update(cx, |palette, cx| palette.refresh(view, inset, metrics, cx));
            if refocus {
                let focus = palette.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            return;
        }
        let sink = crate::palette::sink(self.id, generation, &self.poster);
        let palette = cx.new(|cx| Palette::new(view, inset, metrics, sink, window, cx));
        let focus = palette.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.set_layer(Layer::Palette, Some(palette.clone().into()), cx);
        self.palette = Some(palette);
    }

    /// The window's native picker while one is up.
    #[must_use]
    pub fn picker(&self) -> Option<&Entity<Picker>> {
        self.picker.as_ref()
    }

    /// The window's command palette while it draws one.
    #[must_use]
    pub fn palette(&self) -> Option<&Entity<Palette>> {
        self.palette.as_ref()
    }

    /// The Settings popup while it is up over this window.
    #[must_use]
    pub fn settings(&self) -> Option<&Entity<SettingsView>> {
        self.settings.as_ref().map(|(view, _)| view)
    }

    /// Opens the Settings popup over the window on what the core held (`opening`), taking the keyboard; one already up
    /// takes the keyboard back instead. A modal drawn over the popup keeps the keyboard either way.
    pub fn open_settings(&mut self, opening: Opening, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() {
            let view = SettingsView::open(opening, window, cx);
            let closing = cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
                this.close_settings(window, cx);
            });
            // whatever held the keyboard over the popup and left the window with nothing focused (a GUI ask, a
            // picker or the palette that closed, a control of a page no longer drawn, a menu that closed) hands it
            // back to the front holder, at worst the popup: never the terminal under it, never nothing
            let lost = cx.on_focus_lost(window, |this, window, cx| {
                if this.settings.is_some() {
                    this.focus_terminal(window, cx);
                }
            });
            self.set_layer(Layer::Settings, Some(view.clone().into()), cx);
            self.settings = Some((view, [closing, lost]));
            // the deck stands its focus down while the popup holds the keyboard
            post(&self.poster, |_| {});
        }
        // the popup takes the keyboard unless a picker, GUI ask, palette, popover or alert drawn over it holds it
        self.focus_terminal(window, cx);
    }

    /// Gives the keyboard to the Settings popup when it is up.
    pub fn focus_settings(&self, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.settings() {
            let focus = view.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// Takes the Settings popup down, giving the keyboard back to what holds it without it (the shown terminal).
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_none() {
            return;
        }
        self.set_layer(Layer::Settings, None, cx);
        self.focus_terminal(window, cx);
        post(&self.poster, |_| {});
    }

    /// Hands the Settings popup, while it is up, what the core holds now.
    pub fn sync_settings(&self, snapshot: crate::settings::Snapshot, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.settings() {
            view.update(cx, |view, cx| view.sync(snapshot, window, cx));
        }
    }

    /// Whether the Settings popup holds the window's keyboard.
    fn settings_holds_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.settings()
            .is_some_and(|view| view.read(cx).holds_keyboard(window, cx))
    }

    /// The window's scrollback search bar.
    #[must_use]
    pub fn search_bar(&self) -> &Entity<SearchBar> {
        &self.search
    }

    /// The window's Ctrl-Tab switcher.
    #[must_use]
    pub fn switcher(&self) -> &Entity<SessionSwitcher> {
        &self.switcher
    }

    /// The window's own key handling (module docs); whether it took the press.
    fn key_down(&mut self, stroke: &KeyStroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.settings_holds_keyboard(window, cx) {
            // the popup keeps every key, Ctrl+Tab's too
            return false;
        }
        if let Some(key) = crate::session_switcher::switch_key(stroke)
            && self.switcher.update(cx, |switcher, cx| switcher.press(key, cx))
        {
            return true;
        }
        let focused = |handle: FocusHandle, window: &Window| handle.is_focused(window);
        if let Some(picker) = self
            .picker
            .clone()
            .filter(|picker| focused(picker.read(cx).focus_handle(cx), window))
        {
            let Some(key) = crate::picker::pick_key(stroke) else {
                return false;
            };
            picker.update(cx, |picker, cx| picker.key(key, cx));
            return true;
        }
        if let Some(palette) = self
            .palette
            .clone()
            .filter(|palette| focused(palette.read(cx).focus_handle(cx), window))
        {
            if let Some(key) = crate::picker::pick_key(stroke) {
                palette.update(cx, |palette, cx| palette.key(key, cx));
                return true;
            }
            // a launcher's chord reaches its action from the field: the same chord closes or switches the palette
            let Some(action) =
                crate::keymap_dispatch::menu_action(stroke).filter(|action| crate::palette::LAUNCHERS.contains(action))
            else {
                return false;
            };
            palette.update(cx, |palette, cx| palette.launcher(action, cx));
            return true;
        }
        if focused(self.search.read(cx).focus_handle(cx), window) {
            // Find's own chord closes the bar from its field, as the menu item's toggle does
            let find =
                crate::keymap_dispatch::menu_action(stroke) == Some(agterm_core::keymap::BuiltinAction::ToggleSearch);
            let Some(input) = crate::search::field_key(stroke).or(find.then_some(crate::search::SearchInput::Close))
            else {
                return false;
            };
            self.search.update(cx, |bar, _| bar.send(input));
            return true;
        }
        false
    }

    /// The search bar at the terminal area's top right.
    fn render_search(&self, view: &AnyView, window: &Window, cx: &App) -> AnyElement {
        let viewport = window.viewport_size();
        let area = self.deck.read(cx).anchor_bounds(AnchorTarget::Area);
        let (top, right) = area.map_or((px(0.), px(0.)), |area| {
            (area.top(), (viewport.width - area.right()).max(px(0.)))
        });
        let margin = px(crate::search::MARGIN);
        div()
            .absolute()
            .top(top + margin)
            .right(right + margin)
            .child(view.clone())
            .into_any_element()
    }

    /// The GUI ask over its anchor, the rest of the window taking no clicks.
    fn render_ask(&self, view: &AnyView, cx: &App) -> AnyElement {
        let anchor = self
            .ask
            .as_ref()
            .and_then(|ask| ask.target)
            .and_then(|target| self.deck.read(cx).anchor_bounds(target));
        div()
            .id("gui-ask-layer")
            .absolute()
            .inset_0()
            .occlude()
            .children(anchor.map(|bounds| {
                div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .child(view.clone())
            }))
            .into_any_element()
    }

    /// The model window shown.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn deck(&self) -> &Entity<Deck> {
        &self.deck
    }

    /// The window's title bar.
    #[must_use]
    pub fn titlebar(&self) -> &Entity<Titlebar> {
        &self.titlebar
    }

    /// The window's sidebar.
    #[must_use]
    pub fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The title set last.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Attaches `view` to modal layer `layer` (`None` takes it down).
    pub fn set_layer(&mut self, layer: Layer, view: Option<AnyView>, cx: &mut Context<Self>) {
        self.layers.retain(|(known, _)| *known != layer);
        if let Some(view) = view {
            self.layers.push((layer, view));
            self.layers.sort_by_key(|(known, _)| *known);
        }
        cx.notify();
    }

    /// Whether a modal layer is attached.
    #[must_use]
    pub fn has_layer(&self, layer: Layer) -> bool {
        self.layers.iter().any(|(known, _)| *known == layer)
    }

    /// Closes the window for good: the model closed it, and its terminals with it. Its alerts are cancelled and its
    /// Settings popup goes with it.
    pub fn close(&mut self, window: &mut Window, cx: &mut App) {
        crate::keymap_dispatch::unregister_window(window.window_handle());
        self.closing.set(true);
        self.settings = None;
        self.layers.retain(|(layer, _)| *layer != Layer::Settings);
        crate::modal::dismiss_alerts(window, cx);
        window.remove_window();
    }

    /// Asks whether to close the window and end its sessions (upstream's `windowShouldClose` sheet): "Close “name”?",
    /// Cancel (Escape) / Close (Return, destructive); Close posts [`AppState::close_confirmed`]. A close while it is
    /// up asks nothing more.
    pub fn ask_close(&self, close: &crate::state::WindowClose, poster: Poster, window: &mut Window, cx: &mut App) {
        if self.close_asked.replace(true) {
            return;
        }
        let asked = Rc::clone(&self.close_asked);
        let id = close.window;
        crate::modal::open_alert(
            window,
            close_alert(&close.name, close.sessions),
            move |answer, _, _| {
                asked.set(false);
                if answer.button == CLOSE {
                    post(&poster, move |state: &mut AppState| state.close_confirmed(id));
                }
            },
            cx,
        );
    }

    /// The platform background the window last asked for (opaque, transparent or blurred).
    #[must_use]
    pub fn requested_appearance(&self) -> gpui_kit::WindowBackgroundAppearance {
        self.appearance.get()
    }

    /// Raises the window and gives the keyboard to the shown terminal.
    pub fn present(&self, window: &mut Window, cx: &mut App) {
        window.activate_window();
        self.focus_terminal(window, cx);
    }

    /// Gives the keyboard to what should hold it while the window has it: the window's alert, picker, GUI ask, palette,
    /// popover or Settings popup while one is up, else the shown terminal (a new window's first focus, which the window manager gives the window itself:
    /// a new window maps without an activation request of its own).
    pub fn focus_terminal(&self, window: &mut Window, cx: &mut App) {
        if crate::modal::alert_open(window.window_handle(), cx) {
            crate::modal::refocus_alert(window, cx);
            return;
        }
        let held = self
            .picker
            .as_ref()
            .map(|picker| picker.read(cx).focus_handle(cx))
            .or_else(|| self.ask.as_ref().map(|ask| ask.dialog.read(cx).focus_handle().clone()))
            .or_else(|| self.palette.as_ref().map(|palette| palette.read(cx).focus_handle(cx)))
            .or_else(|| {
                let popovers = self.popovers.read(cx);
                popovers.is_open().then(|| popovers.focus_handle().clone())
            })
            .or_else(|| self.settings().map(|view| view.read(cx).focus_handle(cx)));
        if let Some(handle) = held.or_else(|| self.deck.read(cx).focus_target(cx)) {
            window.focus(&handle, cx);
        }
    }

    /// What `window.list` and the window commands read of this window: full screen and maximized as last painted,
    /// and the size of the display it is on.
    #[must_use]
    pub fn window_state(&self, window: &Window, cx: &App) -> WindowState {
        let pending = self.pending_minimized.get().map(|(desired, _)| desired);
        let (minimized, still_pending) = reported_minimized(read_minimized(window), pending);
        if !still_pending {
            self.pending_minimized.set(None);
        }
        self.painted.get().state(display_size(window, cx), minimized)
    }

    /// On a display that reads the minimized state back, reports `minimized` until the window manager confirms it or
    /// [`MINIMIZE_SETTLE`] passes (the GTK build's `expect_minimized`).
    fn expect_minimized(&self, minimized: bool, window: &Window, cx: &mut App) {
        if read_minimized(window).is_none() {
            return;
        }
        let request = self.minimize_requests.get() + 1;
        self.minimize_requests.set(request);
        self.pending_minimized.set(Some((minimized, request)));
        let pending = Rc::clone(&self.pending_minimized);
        cx.spawn(async move |cx| {
            cx.background_executor().timer(MINIMIZE_SETTLE).await;
            if pending.get().is_some_and(|(_, number)| number == request) {
                pending.set(None);
            }
        })
        .detach();
    }

    /// Renders the model: the deck's pages, the sidebar, the title, the GUI ask, the picker, the palette, the search
    /// bar, the switcher and the popovers' rows. A modal holds the keyboard while the model's modal slot (the window's
    /// picker or GUI ask) or its palette is up, or one of the window's alerts or popovers; while the sidebar's row menu
    /// holds it the deck moves no focus. Whether the core needs a settle for the focus the deck gave.
    #[must_use = "a focus report the core took needs a settle"]
    pub fn sync(&mut self, core: &mut Core, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let id = self.id;
        let dialog = core.modal_pending(id) || core.palette_holds(id);
        let alert = crate::modal::alert_open(window.window_handle(), cx);
        let alert_closed = std::mem::replace(&mut self.alert_up, alert) && !alert;
        self.sync_modals(core, alert_closed, window, cx);
        let popover = self.popovers.read(cx).is_open();
        if alert_closed && !dialog && !popover {
            // an alert that closed over the Settings popup gives it the keyboard back
            self.focus_settings(window, cx);
        }
        let modal = dialog || alert || popover || self.settings.is_some();
        let sidebar = self.sidebar.read(cx);
        // the sidebar's row menu or the app menu holds the keyboard while it is open
        let held = sidebar.menu_open() || self.titlebar.read(cx).menu_open();
        let owner = sidebar.keyboard_owner(cx);
        let focused = self.deck.update(cx, |deck, cx| {
            deck.hold_keyboard(held, owner);
            deck.reconcile(core, modal, window, cx)
        });
        if alert {
            // a menu closing after its item opened the alert may have taken the keyboard back
            crate::modal::refocus_alert(window, cx);
        }
        // after the deck, which may have given a terminal the keyboard: a bar that came up takes it unless a modal
        // holds it
        self.search.update(cx, |bar, cx| bar.sync(core, !modal, window, cx));
        let inset = terminal_area_inset(core, id);
        self.switcher.update(cx, |switcher, cx| switcher.sync(core, inset, cx));
        self.sidebar.update(cx, |sidebar, cx| sidebar.sync(core, cx));
        self.sync_title(core, dialog || alert, window, cx);
        cx.notify();
        focused
    }

    /// The window's dialogs: the GUI ask, the picker, the palette and the popovers. The front one takes the keyboard
    /// back from an alert that closed (`alert_closed`) unless a popover holds it.
    fn sync_modals(&mut self, core: &Core, alert_closed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let refocus = alert_closed && !self.popovers.read(cx).is_open();
        let palette = core.palette_holds(id);
        let place = (terminal_area_inset(core, id), Metrics::of(core.settings()));
        // the palette sits above the modal slot's picker or GUI ask
        self.sync_ask(core, refocus && !palette, window, cx);
        self.sync_picker(core, place, refocus && !palette, window, cx);
        self.sync_palette(core, place, refocus, window, cx);
        self.popovers.update(cx, |popovers, cx| popovers.sync(core, cx));
        self.sync_popover_layer(cx);
    }

    /// The window title and the title bar.
    fn sync_title(&mut self, core: &Core, dialog: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let dashboard = core.dashboard(id).is_some();
        let title = if dashboard {
            dashboard_title(core.library(), id)
        } else {
            window_title(core.library(), Some(id))
        };
        let cover = if dashboard {
            Cover::Dashboard(title.clone())
        } else if core.zoom_target(id).is_some() {
            Cover::Zoom
        } else {
            Cover::None
        };
        let colors = crate::chrome::current(cx);
        self.titlebar
            .update(cx, |bar, cx| bar.sync(core, cover, dialog, colors, cx));
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    /// Applies a window command's request (upstream's `WindowRegistry` actions). GPUI has no client positioning on
    /// Linux, so a move (which this host never claims) is only logged.
    pub fn apply(&self, request: WindowRequest, window: &mut Window, cx: &mut App) {
        match request {
            WindowRequest::Raise(_) => self.present(window, cx),
            WindowRequest::Resize { size: requested, .. } => {
                // leaving the maximized state makes the window take the size now, as `setFrame` resizes a zoomed
                // `NSWindow` at once
                if window.is_maximized() && !window.is_fullscreen() {
                    window.zoom_window();
                }
                #[expect(clippy::cast_precision_loss, reason = "a window side fits an f32")]
                let (width, height) = (requested.width as f32, requested.height as f32);
                window.resize(size(px(width), px(height)));
            }
            WindowRequest::ToggleZoom(_) => window.zoom_window(),
            WindowRequest::ToggleFullscreen(_) => window.toggle_fullscreen(),
            WindowRequest::Minimize { minimized, .. } => {
                if minimized {
                    // a new window parked by its own command leaves frontmost where that command handed it
                    if self.first_focus.get().is_some() {
                        self.first_focus.set(Some(FirstFocus::Parked));
                    }
                    window.minimize_window();
                } else {
                    window.activate_window();
                }
                self.expect_minimized(minimized, window, cx);
            }
            WindowRequest::Move { .. } => tracing::warn!("window.move reached a host that cannot place windows"),
        }
    }

    /// Reports the painted state once a frame in a new one is drawn.
    fn track_painted(&self, window: &mut Window) {
        let now = Painted::of(window);
        if self.painted.get() == now {
            return;
        }
        let (painted, poster, id) = (Rc::clone(&self.painted), self.poster.clone(), self.id);
        let pending = Rc::clone(&self.pending_minimized);
        window.on_next_frame(move |window, cx| {
            let now = Painted::of(window);
            if painted.replace(now) != now {
                let pending = pending.get().map(|(desired, _)| desired);
                let (minimized, _) = reported_minimized(read_minimized(window), pending);
                let state = now.state(display_size(window, cx), minimized);
                post(&poster, move |app: &mut AppState| app.window_state_changed(id, state));
            }
        });
    }
}

impl Render for WindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.track_painted(window);
        let background = crate::chrome::backing(&crate::chrome::current(cx), window, cx);
        let appearance = crate::chrome::translucency(cx).appearance(window.is_fullscreen());
        if self.appearance.replace(appearance) != appearance {
            // upstream's `window.isOpaque` and its CGS blur, as the compositor offers them
            window.set_background_appearance(appearance);
        }
        let layers: Vec<AnyElement> = self
            .layers
            .iter()
            .map(|(layer, view)| match layer {
                Layer::Ask => self.render_ask(view, cx),
                Layer::Search => self.render_search(view, window, cx),
                Layer::Settings => render_settings(view, window, cx),
                _ => div().absolute().inset_0().child(view.clone()).into_any_element(),
            })
            .collect();
        let alert = crate::modal::alert_layer(window.window_handle(), cx);
        div()
            .id("window")
            .capture_key_up(|event: &KeyUpEvent, window, cx| {
                // a release of a press the keymap took is the keymap's: no terminal reports it
                if crate::keymap_dispatch::key_up(event, window, cx) {
                    cx.stop_propagation();
                }
            })
            .on_modifiers_changed(cx.listener(|view, event: &ModifiersChangedEvent, _, cx| {
                // Control went up: a Ctrl-Tab walk selects its highlighted session
                if !event.modifiers.control {
                    view.switcher.update(cx, SessionSwitcher::release);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(self.titlebar.clone())
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_row()
                    .child(self.sidebar.clone())
                    .child(div().flex_1().min_w(px(0.)).h_full().child(self.deck.clone())),
            )
            .children(layers)
            .children(alert)
            .child(crate::chrome::resize_edges(window))
    }
}

/// The Settings popup centered over a dimmed layer that takes every click; one outside the panel closes it.
fn render_settings(view: &AnyView, window: &Window, cx: &Context<WindowView>) -> AnyElement {
    let panel = crate::settings::panel_size(window.viewport_size());
    div()
        .id("settings-layer")
        .absolute()
        .inset_0()
        .occlude()
        .bg(crate::modal::dim())
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
        )
        .child(div().w(panel.width).h(panel.height).child(view.clone()))
        .into_any_element()
}

/// The window's own keys (module docs), which the key monitor runs first: whether the agterm window `window` took the
/// press. A window that is no agterm window (the quick terminal's panel) takes none.
pub fn intercept_key(stroke: &KeyStroke, window: &mut Window, cx: &mut App) -> bool {
    let Some(Some(view)) = window.root::<WindowView>() else {
        return false;
    };
    view.update(cx, |view, cx| view.key_down(stroke, window, cx))
}

/// How long after a window loses the keyboard the monitor checks whether another agterm window took it (the GTK
/// build's `LEAVE_CHECK_DELAY`): the next window's activation comes after this one's deactivation.
const LEAVE_CHECK_DELAY: Duration = Duration::from_millis(150);

/// How long after its first frame a new window's first activation still counts as the focus the window manager grants
/// on mapping it ([`AppState::window_activated`]): that one follows the map at once, while a window left unfocused then
/// is activated later only by the user, whose activation always counts, whatever commands took frontmost before.
const FIRST_FOCUS_GRACE: Duration = Duration::from_secs(1);

/// A window (an agterm window, the quick terminal's panel) lost the keyboard, or the panel removed its own window: once the next activation had its chance,
/// the key monitor forgets its consumed keys when no window of the app holds the keyboard.
pub(crate) fn check_keyboard_left(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(LEAVE_CHECK_DELAY).await;
        cx.update(|cx| {
            if !crate::keymap_dispatch::holds_keyboard(cx.active_window()) {
                crate::keymap_dispatch::keyboard_left();
            }
        });
    })
    .detach();
}

/// The Close button's index in [`close_alert`].
pub const CLOSE: usize = 1;

/// The user's close of a window running `sessions` sessions (upstream's `WindowCloseDelegateProxy.windowShouldClose`).
#[must_use]
pub fn close_alert(name: &str, sessions: usize) -> crate::modal::AlertSpec {
    let plural = if sessions == 1 { "" } else { "s" };
    crate::modal::AlertSpec::confirm(
        format!("Close \u{201c}{name}\u{201d}?"),
        format!(
            "This ends {sessions} running session{plural}. The window can be reopened from File \u{25b8} Open Window."
        ),
        "Close",
    )
}

/// The Clear button's index in [`clear_flags_alert`].
pub const CLEAR: usize = 1;

/// Clear Flagged's confirmation over `count` flagged sessions (upstream's `confirmClearFlags`).
#[must_use]
pub fn clear_flags_alert(count: usize) -> crate::modal::AlertSpec {
    let body = if count == 1 {
        "This unflags 1 session. The session itself is not closed.".to_owned()
    } else {
        format!("This unflags {count} sessions. The sessions themselves are not closed.")
    };
    crate::modal::AlertSpec::confirm("Clear flagged sessions?", body, "Clear")
}

/// Asks over `window` whether to clear its `count` flagged sessions; Clear posts [`AppState::clear_flags_confirmed`].
pub fn ask_clear_flags(id: Uuid, count: usize, poster: Poster, window: &mut Window, cx: &mut App) {
    crate::modal::open_alert(
        window,
        clear_flags_alert(count),
        move |answer, _, _| {
            if answer.button == CLEAR {
                post(&poster, move |state: &mut AppState| state.clear_flags_confirmed(id));
            }
        },
        cx,
    );
}

/// `window`'s frame as it would be saved now: its bounds, or the ones it restores to while maximized or full screen.
/// A minimized X11 window keeps its maximized state, which GPUI reads as not maximized while it is minimized.
#[must_use]
pub fn frame_of(window: &Window) -> crate::window_frames::Frame {
    let (bounds, maximized) = match window.window_bounds() {
        gpui_kit::WindowBounds::Maximized(bounds) => (bounds, true),
        gpui_kit::WindowBounds::Windowed(bounds) | gpui_kit::WindowBounds::Fullscreen(bounds) => (bounds, false),
    };
    crate::window_frames::Frame {
        x: f64::from(f32::from(bounds.origin.x)),
        y: f64::from(f32::from(bounds.origin.y)),
        width: f64::from(f32::from(bounds.size.width)),
        height: f64::from(f32::from(bounds.size.height)),
        maximized: maximized || (read_minimized(window) == Some(true) && read_maximized(window) == Some(true)),
    }
}

/// `window`'s X11 id, `None` for a window of another platform.
#[cfg(target_os = "linux")]
fn x11_id(window: &Window) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
        _ => None,
    }
}

/// Whether the window manager keeps X11 window `window` maximized, minimized or not (GPUI patch `gpui-pre-linux`
/// 0003); `None` elsewhere.
fn read_maximized(window: &Window) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        gpui_linux::x11_window_maximized(x11_id(window)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        None
    }
}

/// The bounds a window opens with: its saved `frame` (maximized again when it closed so), else upstream's default size
/// centered.
fn initial_bounds(frame: Option<crate::window_frames::Frame>, cx: &App) -> gpui_kit::WindowBounds {
    #[expect(clippy::cast_possible_truncation, reason = "a window's logical size fits an f32")]
    let pixels = |value: f64| px(value as f32);
    let Some(frame) = frame else {
        let default = crate::window_frames::DEFAULT_SIZE;
        return gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds::centered(
            None,
            size(pixels(default.width), pixels(default.height)),
            cx,
        ));
    };
    let bounds = gpui_kit::Bounds {
        origin: gpui_kit::point(pixels(frame.x), pixels(frame.y)),
        size: size(pixels(frame.width), pixels(frame.height)),
    };
    if frame.maximized {
        gpui_kit::WindowBounds::Maximized(bounds)
    } else {
        gpui_kit::WindowBounds::Windowed(bounds)
    }
}

/// Opens the window of model window `id` with `app_id` as its Wayland app id / X11 class, when the core's frontmost
/// takes stand at `frontmost_takes`, at its saved `frame` (already constrained to the displays) or upstream's default
/// size; its handle, `None` (logged) when the platform could not open one.
pub fn open(
    id: Uuid,
    frontmost_takes: u64,
    app_id: &str,
    env: &Rc<crate::deck::DeckEnv>,
    frame: Option<crate::window_frames::Frame>,
    cx: &mut App,
) -> Option<gpui_kit::WindowHandle<WindowView>> {
    let min = crate::window_frames::MIN_SIZE;
    #[expect(clippy::cast_possible_truncation, reason = "a window's logical size fits an f32")]
    let min_size = size(px(min.width as f32), px(min.height as f32));
    let options = gpui_kit::WindowOptions {
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(UNTITLED.into()),
            ..gpui_kit::TitlebarOptions::default()
        }),
        window_bounds: Some(initial_bounds(frame, cx)),
        window_min_size: Some(min_size),
        app_id: Some(app_id.to_owned()),
        // the title bar is the app's own (`titlebar.rs`): the window manager's decorations only where the compositor
        // insists (GPUI falls back to them on X11 without a compositing manager, and a Wayland compositor may refuse)
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..gpui_kit::WindowOptions::default()
    };
    let env = Rc::clone(env);
    let poster = env.poster.clone();
    crate::status_glyph::watch_motion(cx);
    let opened = cx.open_window(options, move |window, cx| {
        let deck = cx.new(|_| Deck::new(id, env));
        cx.new(|cx| WindowView::new(id, deck, poster, frontmost_takes, window, cx))
    });
    match opened {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::error!(%error, window = %id, "the window did not open");
            None
        }
    }
}

#[cfg(test)]
mod tests;
