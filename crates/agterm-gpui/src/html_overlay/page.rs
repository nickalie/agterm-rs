//! One page and the handlers that keep the model in step with it: load state, the page and title shown, history, the
//! navigation policy, the hand-off prompt and the bridge (upstream's `HtmlOverlayPage`; port of the GTK build's
//! `html_overlay/page.rs` onto the engine's events).
//!
//! A page is made at its mount and loads at once, shown or not, as the GTK build's web view did; its engine page
//! (`agterm_wpe::ViewId`) exists once the engine runs, which is from the first page a drawn window mounts (the engine
//! renders on the GPU that window's renderer opened, `super::registry`). Every report to the core is a posted
//! main-loop task, never a call in place: the engine's events arrive while the app state may be busy.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use agterm_core::html::bridge::{ADAPTER_SCRIPT, HANDLER_NAME, HELPER_SCRIPT, RELAY_SCRIPT, WORLD};
use agterm_core::html::{
    HtmlLoadState, HtmlNavigation, HtmlNavigationDecision, HtmlOverlay, HtmlOverlayTheme, HtmlPageInfo,
    HtmlReloadTarget, HtmlSource, decide, errors,
};
use agterm_wpe::{
    Event, Geometry, InjectionTime, Load, MessageHandler, NavigationType, PageState as EngineState, PolicyKind,
    PolicyRequest, Rgba, ScriptMessage, SessionId, Settings, UserScript, ViewId, ViewSpec,
};
use gpui_kit::{AnyWindowHandle, App, Entity, WeakEntity};
use url::Url;
use uuid::Uuid;

use super::engine::Web;
use super::panel::PanelColors;
use super::policy::{self, DecisionTarget, LoadEvent, LoadOutcome, LoadTracker, ThemeChange};
use super::registry;
use super::view::PageView;
use crate::modal::{Alert, AlertButton, AlertSpec};
use crate::state::{AppState, Poster, post};

/// The script world the theme runs in, apart from the page's and the bridge's (upstream's `themeWorld`).
const THEME_WORLD: &str = "agterm-theme";

/// The size a page lays out at until its element is laid out.
const FIRST_GEOMETRY: Geometry = Geometry {
    width: 800,
    height: 600,
    scale: 1.0,
};

/// The browser store a page is built on, or why it has none and loads nothing (upstream's `Storage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Storage {
    /// An in-memory store of its own.
    Ephemeral,
    /// The saved store.
    Saved,
    Unavailable(String),
}

/// What the page's handlers share.
struct PageState {
    /// The model's value as last applied.
    overlay: HtmlOverlay,
    applied_revision: u64,
    load: LoadTracker,
    /// The address of the document a browsing page shows; `None` until its first commit.
    shown_url: Option<Url>,
    /// A granted page's grant as a real path, which the scheme's reads are served from.
    grant_root: Option<PathBuf>,
    storage_failure: Option<String>,
    prompt: Option<Entity<Alert>>,
    /// A declined prompt silences the page until real input reaches its view: script can click links in a loop, but
    /// it cannot make native pointer or key events.
    prompts_silenced: bool,
    on_screen: bool,
    life: Life,
    /// What the engine last reported the page shows.
    engine: EngineState,
    /// What this page last posted to the model as its page info: a report goes out unless both it and the model's
    /// value as last applied already hold it, since the model's value lags the posts it has not applied yet.
    reported: Option<HtmlPageInfo>,
}

impl PageState {
    fn active(&self) -> bool {
        self.life != Life::Released
    }
}

/// Where a page is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Life {
    Active,
    /// The page's `window.close()` asked for its close. It stays asked while the session sits in an undoable close,
    /// where no slot is found to close, so a restored page closes at its next apply.
    CloseRequested,
    /// Released: the handlers still reached do nothing.
    Released,
}

/// One page (upstream's `HtmlOverlayPage`).
pub(super) struct Page {
    id: Uuid,
    /// The overlay's own background (`--background-color`), which replaces the theme's.
    background: Option<String>,
    colors: RefCell<PanelColors>,
    /// The theme the page's scripts carry.
    theme: RefCell<HtmlOverlayTheme>,
    poster: Poster,
    state: RefCell<PageState>,
    storage: Storage,
    /// The engine's page, once the engine runs.
    view: Cell<Option<ViewId>>,
    web: RefCell<Option<Rc<dyn Web>>>,
    /// The element showing the page, once mounted in a window.
    element: RefCell<Option<WeakEntity<PageView>>>,
    /// The window the element was made in: a session moving to another window gets a new element there.
    element_window: Cell<Option<AnyWindowHandle>>,
    /// The window the page was last drawn in, which its prompts open over.
    window: Cell<Option<AnyWindowHandle>>,
    /// The size and scale the element was last laid out at.
    geometry: Cell<Option<Geometry>>,
    /// The element holds the keyboard: the engine's page is told so, also when it is made later.
    focused: Cell<bool>,
    /// The input method's composition in progress (its text and caret), given again to an engine page made after it
    /// began.
    preedit: RefCell<Option<(String, u32)>>,
    me: Weak<Page>,
}

impl Page {
    /// The page for `overlay` (its own `background` replacing the theme's), built on `storage`.
    pub(super) fn new(
        overlay: &HtmlOverlay,
        background: Option<&str>,
        colors: PanelColors,
        storage: Storage,
        poster: &Poster,
    ) -> Rc<Self> {
        let storage_failure = match &storage {
            Storage::Unavailable(failure) => Some(failure.clone()),
            _ => None,
        };
        Rc::new_cyclic(|me| Self {
            id: overlay.id,
            background: background.map(str::to_owned),
            theme: RefCell::new(colors.theme()),
            colors: RefCell::new(colors),
            poster: poster.clone(),
            storage,
            state: RefCell::new(PageState {
                overlay: overlay.clone(),
                applied_revision: overlay.reload_revision,
                load: LoadTracker::default(),
                shown_url: None,
                grant_root: None,
                storage_failure,
                prompt: None,
                prompts_silenced: false,
                on_screen: false,
                life: Life::Active,
                engine: EngineState::default(),
                reported: None,
            }),
            view: Cell::new(None),
            web: RefCell::new(None),
            element: RefCell::new(None),
            element_window: Cell::new(None),
            window: Cell::new(None),
            geometry: Cell::new(None),
            focused: Cell::new(false),
            preedit: RefCell::new(None),
            me: me.clone(),
        })
    }

    pub(super) fn id(&self) -> Uuid {
        self.id
    }

    pub(super) fn poster(&self) -> &Poster {
        &self.poster
    }

    pub(super) fn view_id(&self) -> Option<ViewId> {
        self.view.get()
    }

    pub(super) fn uses_saved_store(&self) -> bool {
        self.storage == Storage::Saved
    }

    pub(super) fn storage(&self) -> &Storage {
        &self.storage
    }

    pub(super) fn grant_root(&self) -> Option<PathBuf> {
        self.state.borrow().grant_root.clone()
    }

    pub(super) fn overlay(&self) -> HtmlOverlay {
        self.state.borrow().overlay.clone()
    }

    pub(super) fn colors(&self) -> PanelColors {
        self.colors.borrow().clone()
    }

    /// Why the page cannot load, for good (the unit tests).
    #[cfg(test)]
    pub(super) fn failure(&self) -> Option<String> {
        self.state.borrow().storage_failure.clone()
    }

    pub(super) fn on_screen(&self) -> bool {
        self.state.borrow().on_screen
    }

    pub(super) fn active(&self) -> bool {
        self.state.borrow().active()
    }

    pub(super) fn web(&self) -> Option<Rc<dyn Web>> {
        self.web.borrow().clone()
    }

    /// The page's element is now `element`, made in `window` (its focus listeners are that window's).
    pub(super) fn set_element(&self, element: WeakEntity<PageView>, window: AnyWindowHandle) {
        self.element.replace(Some(element));
        self.element_window.set(Some(window));
    }

    /// The window the element was made in.
    pub(super) fn element_window(&self) -> Option<AnyWindowHandle> {
        self.element_window.get()
    }

    pub(super) fn element(&self) -> Option<Entity<PageView>> {
        self.element.borrow().as_ref().and_then(WeakEntity::upgrade)
    }

    /// The page was drawn in `window`.
    pub(super) fn drawn_in(&self, window: AnyWindowHandle) {
        self.window.set(Some(window));
    }

    /// Has the engine draw the page anew (a new element shows nothing until it does): hidden and shown again while on
    /// screen, else at its next showing; the geometry goes again with the element's first layout.
    pub(super) fn redraw(&self) {
        self.geometry.set(None);
        if let (Some(web), Some(view), true) = (self.web(), self.view.get(), self.on_screen()) {
            web.set_visible(view, false);
            web.set_visible(view, true);
        }
    }

    /// The element took or lost the keyboard: the engine's page follows, now or once it is made.
    pub(super) fn set_focused(&self, focused: bool) {
        self.focused.set(focused);
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.set_focused(view, focused);
        }
    }

    /// The input method's composition is `text` with the caret `cursor` characters in (empty ends it): the engine's
    /// page follows, now or once it is made.
    pub(super) fn ime_preedit(&self, text: String, cursor: u32) {
        self.preedit.replace((!text.is_empty()).then(|| (text.clone(), cursor)));
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.ime_preedit(view, text, cursor);
        }
    }

    /// Text typed or committed by the input method goes in, ending a composition. Before the engine's page exists
    /// there is nothing to type into, as for keys.
    pub(super) fn ime_commit(&self, text: String) {
        self.preedit.replace(None);
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.ime_commit(view, text);
        }
    }

    /// The element was laid out at `geometry`: the engine's page follows when it changed.
    pub(super) fn set_geometry(&self, geometry: Geometry) {
        if self.geometry.replace(Some(geometry)) == Some(geometry) {
            return;
        }
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.set_geometry(view, geometry);
        }
    }

    /// Redraws the element: the model or the engine changed what it shows.
    fn refresh(&self, cx: &mut App) {
        if let Some(element) = self.element() {
            element.update(cx, |_, cx| cx.notify());
        }
    }

    // MARK: - the engine's page

    /// The engine could not start: the page fails with the reason, and is not realized later.
    pub(super) fn engine_failed(&self, failure: &str) {
        let first = {
            let mut state = self.state.borrow_mut();
            let first = state.storage_failure.is_none();
            state.storage_failure = Some(failure.to_owned());
            first
        };
        if first {
            self.fail(failure.to_owned());
        }
    }

    /// Makes the engine's page on `web`, `session` its store (none: the store is unavailable, and the page loads
    /// nothing), at `zoom`, and loads the source. What the page took on while it waited goes with it: the size and
    /// scale, the theme and scripts, the zoom (in its spec), whether it shows, holds the keyboard and an input method
    /// composes in it, and the load the model asks for last; input events before it are gone, as for a page that
    /// does not exist.
    pub(super) fn realize(&self, web: &Rc<dyn Web>, session: Option<SessionId>, zoom: f64) {
        if self.view.get().is_some() || !self.active() {
            return;
        }
        self.web.replace(Some(Rc::clone(web)));
        let Some(session) = session else {
            self.load_original();
            return;
        };
        let overlay = self.overlay();
        let themed = overlay.themed();
        let spec = ViewSpec {
            session,
            settings: Settings::overlay(overlay.flags.javascript),
            scripts: self.scripts(&overlay),
            message_handlers: if themed {
                vec![MessageHandler {
                    name: HANDLER_NAME.to_owned(),
                    world: Some(WORLD.to_owned()),
                }]
            } else {
                Vec::new()
            },
            zoom,
            // a file page's unstyled canvas shows the themed panel behind it while authored backgrounds still paint;
            // a URL page keeps the browser's opaque canvas, which a web app styled against it relies on
            background: themed.then_some(Rgba::TRANSPARENT),
            geometry: self.geometry.get().unwrap_or(FIRST_GEOMETRY),
        };
        let view = web.create_view(spec);
        self.view.set(Some(view));
        registry::page_view_created(self.id, view);
        web.set_visible(view, self.on_screen());
        // the element may have taken the keyboard while the engine started: GPUI tells it once, then never again
        if self.focused.get() {
            web.set_focused(view, true);
            if let Some((text, cursor)) = self.preedit.borrow().clone() {
                web.ime_preedit(view, text, cursor);
            }
        }
        self.load_original();
    }

    /// The theme, and on a file page the bridge's scripts (upstream's `installScripts`).
    fn scripts(&self, overlay: &HtmlOverlay) -> Vec<UserScript> {
        let themed = overlay.themed();
        let script = |source: &str, time: InjectionTime, world: Option<&str>| UserScript {
            source: source.to_owned(),
            time,
            world: world.map(str::to_owned),
        };
        let mut scripts = vec![script(
            &self.theme.borrow().script(themed),
            InjectionTime::Start,
            Some(THEME_WORLD),
        )];
        if !themed {
            return scripts;
        }
        if overlay.flags.javascript {
            scripts.push(script(RELAY_SCRIPT, InjectionTime::Start, Some(WORLD)));
            scripts.push(script(HELPER_SCRIPT, InjectionTime::Start, None));
        }
        scripts.push(script(ADAPTER_SCRIPT, InjectionTime::End, Some(WORLD)));
        scripts
    }

    /// Follows the terminal's `colors` (upstream's `applyTheme`): the panel is redrawn in them, and a changed theme is
    /// given to later loads; a file page wears it at once by reloading what it shows, a URL page gets the variables
    /// alone at its next load. Nothing runs in the live document.
    pub(super) fn apply_theme(&self, terminal: &super::panel::TerminalColors, cx: &mut App) {
        if !self.active() {
            return;
        }
        let overlay = self.overlay();
        let colors = PanelColors::resolve(terminal, self.background.as_deref());
        self.colors.replace(colors.clone());
        self.refresh(cx);
        let next = colors.theme();
        let change = policy::theme_change(&self.theme.borrow(), &next, overlay.themed());
        if change == ThemeChange::Unchanged {
            return;
        }
        self.theme.replace(next);
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.set_scripts(view, self.scripts(&overlay));
        }
        if change == ThemeChange::ReinstallAndReload {
            self.reload_shown();
        }
    }

    // MARK: - engine events

    /// One of the engine's events about this page.
    pub(super) fn event(&self, event: &Event, cx: &mut App) {
        match event {
            Event::Load { event, .. } => match event {
                agterm_wpe::LoadEvent::Started => self.load_event(&LoadEvent::Started),
                agterm_wpe::LoadEvent::Committed => self.committed(),
                agterm_wpe::LoadEvent::Finished => self.load_event(&LoadEvent::Finished),
                agterm_wpe::LoadEvent::Redirected => {}
            },
            Event::LoadFailed { error, .. } => self.load_event(&policy::failure_event(error)),
            Event::WebProcessTerminated { .. } => {
                self.state.borrow_mut().shown_url = None;
                self.load_event(&LoadEvent::Terminated);
            }
            Event::Page { page, .. } => self.engine_page(page),
            Event::Policy(request) => self.decide_policy(request, cx),
            Event::ScriptMessage(message) => self.bridge_message(message),
            Event::CloseRequested { .. } => {
                {
                    let mut state = self.state.borrow_mut();
                    if state.life == Life::Active {
                        state.life = Life::CloseRequested;
                    }
                }
                self.close_if_requested();
            }
            Event::ViewFailed { message, .. } => {
                tracing::warn!(page = %self.id, message, "the web engine could not build the page");
                self.fail(message.clone());
            }
            _ => {}
        }
    }

    // MARK: - the bridge

    /// Runs one request the page sent and answers it once (upstream's `handleBridgeRequest`): the core resolves the
    /// page where it sits when the task runs, so a page that left its slot is refused before anything runs.
    fn bridge_message(&self, message: &ScriptMessage) {
        let Some(web) = self.web() else { return };
        if !self.active() {
            web.reply_script_message(message.token, Err("page closed".to_owned()));
            return;
        }
        let Some(body) = message.body.clone() else {
            web.reply_script_message(message.token, Err("invalid request".to_owned()));
            return;
        };
        let token = registry::park(message.token);
        let id = self.id;
        post(&self.poster, move |state: &mut AppState| {
            match state.core.html_bridge_request(id, body.as_bytes()) {
                Ok(request) => state.handle_page_request(&request, token),
                Err(message) => super::reject(token, &message),
            }
        });
    }

    // MARK: - load state and the page shown

    fn load_event(&self, event: &LoadEvent) {
        let outcome = {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.load.handle(event)
        };
        self.report_outcome(outcome);
    }

    fn committed(&self) {
        let web = self.web_url();
        {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.load.handle(&LoadEvent::Committed);
            state.shown_url = web;
        }
        self.report_page();
    }

    fn report_outcome(&self, outcome: LoadOutcome) {
        match outcome {
            LoadOutcome::None => {}
            LoadOutcome::Loading => self.report_load(HtmlLoadState::Loading, None),
            LoadOutcome::Loaded => {
                self.report_page();
                self.report_load(HtmlLoadState::Loaded, None);
            }
            LoadOutcome::Failed(message) => self.report_load(HtmlLoadState::Failed, Some(message)),
        }
    }

    fn report_load(&self, load: HtmlLoadState, error: Option<String>) {
        {
            let state = self.state.borrow();
            if state.overlay.load_state == load && state.overlay.load_error == error {
                return;
            }
        }
        let id = self.id;
        post(&self.poster, move |state: &mut AppState| {
            if state.core.set_html_load_state(id, load, error) {
                state.settle();
            }
        });
    }

    fn fail(&self, message: String) {
        let outcome = self.state.borrow_mut().load.fail(message);
        self.report_outcome(outcome);
    }

    /// The engine's address as the app spells it.
    fn web_url(&self) -> Option<Url> {
        let uri = self.state.borrow().engine.uri.clone()?;
        Url::parse(&policy::app_uri(&uri)).ok()
    }

    /// The engine reported what the page shows: a new address is a URL change (upstream's `urlChanged`), anything
    /// else (the title, the history) a page report.
    fn engine_page(&self, page: &EngineState) {
        let moved = {
            let mut state = self.state.borrow_mut();
            let moved = state.engine.uri != page.uri;
            state.engine = page.clone();
            moved
        };
        if moved {
            let web = self.web_url();
            let mut state = self.state.borrow_mut();
            state.shown_url = policy::shown_after_change(state.shown_url.as_ref(), web.as_ref());
        }
        self.report_page();
    }

    /// Reports what the page shows and where its history can go (upstream's `reportPage`).
    fn report_page(&self) {
        if self.state.borrow().engine.uri.is_none() {
            return;
        }
        let web = self.web_url();
        let info = {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            let Some(url) = policy::page_url(&state.overlay, state.shown_url.as_ref(), web.as_ref()) else {
                return;
            };
            let info = policy::page_info(
                &url,
                state.engine.title.as_deref(),
                state.engine.can_go_back,
                state.engine.can_go_forward,
            );
            if !needs_report(state.overlay.current.as_ref(), state.reported.as_ref(), &info) {
                return;
            }
            state.reported = Some(info.clone());
            info
        };
        let id = self.id;
        post(&self.poster, move |state: &mut AppState| {
            if state.core.set_html_page(id, Some(info)) {
                state.settle();
            }
        });
    }

    // MARK: - the model's value

    /// Takes the model's latest value and whether the page is on screen; reloads when its revision moved (upstream's
    /// `apply` and `setOnScreen`). A page out of sight asks nothing, and its pending prompt goes with it.
    pub(super) fn apply(&self, overlay: &HtmlOverlay, on_screen: bool, cx: &mut App) {
        let (reload, shown_changed) = {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.overlay = overlay.clone();
            let shown_changed = state.on_screen != on_screen;
            state.on_screen = on_screen;
            let moved = overlay.reload_revision != state.applied_revision;
            state.applied_revision = overlay.reload_revision;
            (moved.then_some(overlay.reload_target), shown_changed)
        };
        if shown_changed && let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            // a page out of sight renders nothing
            web.set_visible(view, on_screen);
        }
        self.refresh(cx);
        if !on_screen {
            self.end_prompt(cx);
        }
        self.close_if_requested();
        match reload {
            Some(HtmlReloadTarget::Current) => self.reload_shown(),
            Some(HtmlReloadTarget::Original) => self.load_original(),
            None => {}
        }
    }

    /// Reloads what the page shows; before a first commit the engine has nothing to reload, so the source loads again.
    fn reload_shown(&self) {
        let reload = {
            let mut state = self.state.borrow_mut();
            let reload = !state.overlay.text_loaded() && state.load.committed();
            if reload {
                state.load.begin();
            }
            reload
        };
        match (reload, self.web(), self.view.get()) {
            (true, Some(web), Some(view)) => web.reload(view),
            _ => self.load_original(),
        }
    }

    /// Loads the page's source (upstream's `loadOriginal`): a URL as is, a granted file through the grant's scheme, a
    /// file without a grant from its text at `about:blank`; a page without its store loads nothing and fails.
    fn load_original(&self) {
        let source = {
            let mut state = self.state.borrow_mut();
            if let Some(failure) = state.storage_failure.clone() {
                drop(state);
                self.fail(failure);
                return;
            }
            state.load.begin();
            state.overlay.source.clone()
        };
        let load = match source {
            HtmlSource::Url(url) => Load::Uri(url.as_str().to_owned()),
            HtmlSource::File {
                path,
                grant_root: Some(root),
            } => {
                self.state.borrow_mut().grant_root = std::fs::canonicalize(&root).ok();
                match policy::granted_uri(&path) {
                    Some(uri) => Load::Uri(uri),
                    None => return self.fail(format!("not an absolute path: {path}")),
                }
            }
            HtmlSource::File { path, grant_root: None } => match std::fs::read_to_string(&path) {
                Ok(html) => Load::Html { html, base_uri: None },
                Err(error) => return self.fail(error.to_string()),
            },
        };
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.load(view, load);
        }
    }

    pub(super) fn set_zoom(&self, zoom: f64) {
        if let (Some(web), Some(view)) = (self.web(), self.view.get()) {
            web.set_zoom(view, zoom);
        }
    }

    // MARK: - navigation and hand-offs

    /// A history step or hand-off (upstream's `navigate`).
    pub(super) fn navigate(&self, navigation: HtmlNavigation) -> Result<(), String> {
        let (web, view) = (self.web(), self.view.get());
        let engine = self.state.borrow().engine.clone();
        match navigation {
            HtmlNavigation::Back => {
                let (Some(web), Some(view), true) = (web, view, engine.can_go_back) else {
                    return Err(errors::no_history(navigation));
                };
                web.go_back(view);
            }
            HtmlNavigation::Forward => {
                let (Some(web), Some(view), true) = (web, view, engine.can_go_forward) else {
                    return Err(errors::no_history(navigation));
                };
                web.go_forward(view);
            }
            HtmlNavigation::Browser => {
                let opened = self
                    .browser_url()
                    .is_some_and(|url| registry::open_in_browser(url.as_str()));
                if !opened {
                    return Err(errors::NO_BROWSER.to_owned());
                }
            }
            HtmlNavigation::Finder => {
                if !self.overlay().themed() {
                    return Err(errors::FINDER_REQUIRES_FILE.to_owned());
                }
                if let Some(path) = self.page_path() {
                    post(&self.poster, move |state: &mut AppState| state.reveal(path));
                }
            }
        }
        Ok(())
    }

    /// What a hand-off to the browser opens (upstream's `browserURL`).
    pub(super) fn browser_url(&self) -> Option<Url> {
        let web = self.web_url();
        let state = self.state.borrow();
        policy::browser_url(&state.overlay, state.shown_url.as_ref(), web.as_ref())
    }

    /// The file the page shows, which the file manager reveals.
    fn page_path(&self) -> Option<PathBuf> {
        let web = self.web_url();
        let state = self.state.borrow();
        policy::page_url(&state.overlay, state.shown_url.as_ref(), web.as_ref())?
            .to_file_path()
            .ok()
    }

    // MARK: - the navigation policy

    /// A navigation or response question, answered at once (`WebKit` holds it until then).
    fn decide_policy(&self, request: &PolicyRequest, cx: &mut App) {
        let Some(web) = self.web() else { return };
        let allow = match &request.kind {
            PolicyKind::Navigation {
                uri,
                navigation,
                new_window,
            } => {
                let clicked = *navigation == NavigationType::LinkClicked;
                let target = if *new_window {
                    DecisionTarget::NewWindow
                } else {
                    DecisionTarget::Frame
                };
                policy::navigation_proceeds(uri, *new_window, || self.allows(uri, target, clicked, cx))
            }
            PolicyKind::Response {
                uri,
                main_frame,
                mime_supported,
            } => self.decide_response(uri, *main_frame, *mime_supported, cx),
        };
        web.decide_policy(request.token, allow);
    }

    /// A response: nothing is saved to disk (the engine reports an unshowable main resource as interrupted), and the
    /// main frame's address is checked against the policy, which a refused one fails while a load is pending.
    fn decide_response(&self, uri: &str, main: bool, mime_supported: bool, cx: &mut App) -> bool {
        if !mime_supported {
            return false;
        }
        let target = if main {
            DecisionTarget::MainResponse
        } else {
            DecisionTarget::SubframeResponse
        };
        if self.allows(uri, target, false, cx) {
            return true;
        }
        if main && self.active() {
            let outcome = self.state.borrow_mut().load.blocked(&policy::app_uri(uri));
            self.report_outcome(outcome);
        }
        false
    }

    /// Whether the shared policy allows `uri`; a hand-off it offers is asked about.
    fn allows(&self, uri: &str, target: DecisionTarget, clicked: bool, cx: &mut App) -> bool {
        let Ok(url) = Url::parse(&policy::app_uri(uri)) else {
            return false;
        };
        let decision = {
            let state = self.state.borrow();
            if !state.active() {
                return false;
            }
            decide(&policy::navigation_action(url.clone(), target, clicked), &state.overlay)
        };
        if decision == HtmlNavigationDecision::OpenExternal {
            let page = self.me.clone();
            cx.defer(move |cx| {
                if let Some(page) = page.upgrade() {
                    page.ask_to_open(url, cx);
                }
            });
        }
        decision == HtmlNavigationDecision::Allow
    }

    /// A page's own hand-off asks first, one prompt at a time, only while the page is on screen; a request while one
    /// is up is dropped (upstream's `askToOpen`). Cancel is the default, so typing meant for the page cannot approve
    /// a surprise prompt.
    fn ask_to_open(&self, url: Url, cx: &mut App) {
        {
            let state = self.state.borrow();
            if !state.active() || !state.on_screen || state.prompt.is_some() || state.prompts_silenced {
                return;
            }
        }
        let Some(window) = self.window.get() else {
            return;
        };
        let shown = HtmlSource::origin_of(&url).unwrap_or_else(|| url.as_str().to_owned());
        let spec = AlertSpec {
            heading: format!("Open {shown} in your browser?"),
            body: url.as_str().to_owned(),
            buttons: vec![
                AlertButton::cancel("Cancel").as_default(),
                AlertButton::plain("Open").suggested(),
            ],
            checks: Vec::new(),
            field: None,
        };
        let page = self.me.clone();
        let opened = window.update(cx, |_, window, cx| {
            crate::modal::open_alert(
                window,
                spec,
                move |answer, _, _| {
                    if let Some(page) = page.upgrade() {
                        page.answer_prompt(&url, answer.button == 1);
                    }
                },
                cx,
            )
        });
        if let Ok(alert) = opened {
            self.state.borrow_mut().prompt = Some(alert);
        }
    }

    fn answer_prompt(&self, url: &Url, approved: bool) {
        {
            let mut state = self.state.borrow_mut();
            if state.prompt.take().is_none() {
                return;
            }
            if !approved {
                state.prompts_silenced = true;
            }
        }
        if approved && !registry::open_in_browser(url.as_str()) {
            tracing::warn!("no default web browser to open the link in");
        }
    }

    /// Withdraws the page's prompt, unanswered.
    fn end_prompt(&self, cx: &mut App) {
        let prompt = self.state.borrow_mut().prompt.take();
        if let (Some(alert), Some(window)) = (prompt, self.window.get()) {
            let _ = window.update(cx, |_, window, cx| crate::modal::withdraw_alert(&alert, window, cx));
        }
    }

    fn close_if_requested(&self) {
        if self.state.borrow().life != Life::CloseRequested {
            return;
        }
        let id = self.id;
        post(&self.poster, move |state: &mut AppState| {
            if state.core.close_html_overlay(id) {
                state.settle();
            }
        });
    }

    // MARK: - input

    /// Typing and clicks are user activity, so auto-follow cannot switch sessions under them, and real input lifts a
    /// declined prompt's silence; a click on a pane page moves the split's focus to its pane, and one on a session-wide
    /// page beside a pane ask to the pane the ask leaves uncovered (upstream's `onUserInput`, `onFocus`, `onClick`).
    pub(super) fn user_input(&self, click: bool) {
        {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.prompts_silenced = false;
        }
        let id = self.id;
        post(&self.poster, move |state: &mut AppState| {
            let Some(slot) = state.core.html_overlay_slot(id) else {
                return;
            };
            state.core.note_user_activity(slot.session);
            let moved = click && (state.core.html_page_focused(id) | state.core.html_page_clicked(id));
            if moved {
                state.settle();
            }
            state.page_input();
        });
    }

    /// The page took the keyboard (the user's click, or the deck): a pane page's split focus follows it.
    pub(super) fn focused(&self) {
        if !self.active() {
            return;
        }
        let id = self.id;
        post(&self.poster, move |state: &mut AppState| {
            if state.core.html_page_focused(id) {
                state.settle();
            }
        });
    }

    // MARK: - release

    /// Ends the page (upstream's `close`): its prompt and its engine page go, and the handlers still reached do
    /// nothing.
    pub(super) fn close(&self, cx: &mut App) {
        self.end_prompt(cx);
        self.state.borrow_mut().life = Life::Released;
        if let (Some(web), Some(view)) = (self.web(), self.view.take()) {
            web.close_view(view);
        }
        if let Some(element) = self.element() {
            element.update(cx, PageView::release);
        }
    }
}

/// Whether a page report of `info` goes out: unless the model's value as last applied (`model`) and the page's last
/// post (`reported`) both hold it. The model alone lags: a report posted but not yet applied leaves it at the value
/// before, so a reload's title reported cleared and then back would read as unchanged and the title stay cleared.
fn needs_report(model: Option<&HtmlPageInfo>, reported: Option<&HtmlPageInfo>, info: &HtmlPageInfo) -> bool {
    model != Some(info) || reported != Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: Option<&str>) -> HtmlPageInfo {
        HtmlPageInfo {
            page: "/tmp/page.html".to_owned(),
            title: title.map(str::to_owned),
            can_go_back: false,
            can_go_forward: false,
        }
    }

    #[test]
    fn a_report_the_model_has_not_applied_yet_does_not_hide_the_next_one() {
        let titled = info(Some("Page"));
        let cleared = info(None);
        // settled: the model and the last post hold it
        assert!(!needs_report(Some(&titled), Some(&titled), &titled));
        // a reload cleared the title (posted, not applied yet), then the title came back
        assert!(needs_report(Some(&titled), Some(&cleared), &titled));
        // the model moved on its own (a reload's reset) while the last post still holds it
        assert!(needs_report(Some(&cleared), Some(&titled), &titled));
        // the first report
        assert!(needs_report(None, None, &titled));
    }
}
