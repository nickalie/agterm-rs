//! The pages by id, the engine, the browser stores pages are built on, the grant's URI scheme and the page requests
//! waiting on an answer (upstream's `HtmlOverlayRegistry`; port of the GTK build's `html_overlay/registry.rs`). It
//! lives on GPUI's main thread; every borrow is short and never spans a call into a page, which may come back here.
//!
//! The engine (`ngterm_wpe::Host`, one per process) starts with the first page once a window has drawn: it renders on
//! the GPU that window's renderer opened, with the formats that device imports (`crate::external_texture::negotiate`),
//! which exist only from then on. The start runs on a thread of its own (a vendored WPE `WebKit`'s engine starts in a
//! worker process, `ngterm_wpe::worker`), so the main thread never waits on it and a quit meanwhile waits for nothing;
//! a page mounted before it ends stays waiting and is realized once it does. Its events come back on one channel the
//! pump ([`init`]) dispatches on the main thread; should they end while the app runs (the worker died), every page
//! fails for the rest of the run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, AppContext as _, ClipboardItem, Entity, Window};
use ngterm_core::Core;
use ngterm_core::html::bridge;
use ngterm_core::html::profile::{self, BrowserProfile};
use ngterm_core::html::{ClearProgress, HtmlHost, HtmlNavigation, HtmlOverlay, HtmlSource, StoreClear, errors};
use ngterm_core::pane_env::uuid_string;
use ngterm_proto::Response;
use ngterm_wpe::{
    BufferFormat, DmaBufTarget, Event, FrameMode, HostConfig, ReplyToken, SchemeError, SchemeResponse, SessionId,
    SessionSpec, ViewId,
};
use uuid::Uuid;

use super::engine::{Engine, Web};
use super::page::{Page, Storage};
use super::panel::{PanelColors, TerminalColors};
use super::policy::{self, GRANT_SCHEME};
use super::view::PageView;
use crate::external_texture::{FrameSource, negotiate};
use crate::state::Poster;

/// Why a page waits: no window has drawn yet, so the engine cannot know the GPU to render on.
const NOT_READY: &str = "the web engine starts once a window has drawn";
/// Why a page waits: the engine is starting.
const STARTING: &str = "the web engine is starting";
/// Why every page fails once the engine ended while the app ran (its worker died).
const ENDED: &str = "the web engine ended unexpectedly; restart ngterm to show pages again";

/// Where the engine stands.
enum EngineState {
    /// Not started yet.
    Idle,
    /// Starting on a thread of its own, so a slow start (a worker's) never holds the main thread, and a quit meanwhile
    /// waits for nothing.
    Starting,
    Running(Rc<dyn Web>),
    /// The start failed, or the engine ended, for good (the engine starts once per process).
    Failed(String),
}

/// What the start thread hands the pump.
type Started = Result<(ngterm_wpe::Host, async_channel::Receiver<Event>), ngterm_wpe::StartError>;

struct Registry {
    pages: HashMap<Uuid, Rc<Page>>,
    by_view: HashMap<ViewId, Uuid>,
    engine: EngineState,
    /// The page zoom every page shows at; `None` before the first settle (actual size).
    zoom: Option<f64>,
    /// Where the browser profile and the saved store live.
    state_dir: Option<PathBuf>,
    /// The saved store's directories once the profile is read, and the store once the engine made it.
    saved_dirs: Option<(PathBuf, PathBuf)>,
    saved: Option<SessionId>,
    /// The in-memory store of each page that has one.
    ephemeral: HashMap<Uuid, SessionId>,
    /// The engine is emptying the saved store (upstream's `clearing`): no page may use it meanwhile.
    clearing: bool,
    /// Why the engine could not empty the saved store, kept for the poll that answers the clear.
    clear_failure: Option<String>,
    /// The page requests waiting on the core's answer, by the token the core is handed.
    replies: HashMap<u64, ReplyToken>,
    next_reply: u64,
    /// The terminal's colors the pages are drawn in.
    colors: TerminalColors,
    poster: Option<Poster>,
    /// Where the engine's start goes from its thread: the pump [`init`] spawned.
    pump: Option<async_channel::Sender<Started>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            pages: HashMap::new(),
            by_view: HashMap::new(),
            engine: EngineState::Idle,
            zoom: None,
            state_dir: None,
            saved_dirs: None,
            saved: None,
            ephemeral: HashMap::new(),
            clearing: false,
            clear_failure: None,
            replies: HashMap::new(),
            next_reply: 0,
            colors: TerminalColors::default(),
            poster: None,
            pump: None,
        }
    }
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::default();
}

fn with<R>(change: impl FnOnce(&mut Registry) -> R) -> R {
    REGISTRY.with(|registry| change(&mut registry.borrow_mut()))
}

fn page(id: Uuid) -> Option<Rc<Page>> {
    with(|registry| registry.pages.get(&id).cloned())
}

fn page_of_view(view: ViewId) -> Option<Rc<Page>> {
    with(|registry| {
        let id = registry.by_view.get(&view)?;
        registry.pages.get(id).cloned()
    })
}

/// The core's way to the pages (upstream's registry calls in `ControlServer+SessionActions.swift`).
struct PageHost;

impl HtmlHost for PageHost {
    fn web_views_unavailable(&self) -> Option<String> {
        super::sandbox::failure().or_else(|| with(|registry| engine_failure(registry)))
    }

    fn persistent_store_failure(&self) -> Option<String> {
        persistent_store_failure()
    }

    fn navigate(&self, id: Uuid, navigation: HtmlNavigation) -> Result<(), String> {
        navigate(id, navigation)
    }

    fn clear_saved_store(&self) -> Result<StoreClear, String> {
        clear_saved_store()
    }

    fn saved_store_clear(&self) -> ClearProgress {
        with(|registry| {
            if registry.clearing {
                ClearProgress::Running
            } else {
                registry
                    .clear_failure
                    .take()
                    .map_or(ClearProgress::Done, ClearProgress::Failed)
            }
        })
    }
}

fn engine_failure(registry: &Registry) -> Option<String> {
    match &registry.engine {
        EngineState::Failed(failure) => Some(failure.clone()),
        _ => None,
    }
}

/// Makes the registry the core's page host, its saved store kept under `state_dir`, its reports posted through
/// `poster` (upstream's `install` and `profile`).
pub fn install(core: &mut Core, state_dir: PathBuf, poster: Poster) {
    with(|registry| {
        registry.state_dir = Some(state_dir);
        registry.poster = Some(poster);
    });
    core.set_html_host(Box::new(PageHost));
}

/// Spawns the pump: it takes the engine's start from the start thread ([`engine`]), then dispatches the engine's events
/// on the main thread, and fails every page when the engine ends while the app runs.
pub fn init(cx: &mut App) {
    let (pump, starts) = async_channel::bounded::<Started>(1);
    with(|registry| registry.pump = Some(pump));
    cx.spawn(async move |cx| {
        let Ok(started) = starts.recv().await else { return };
        let Some(events) = cx.update(|_| started_engine(started)) else {
            return;
        };
        while let Ok(event) = events.recv().await {
            cx.update(|cx| dispatch(&event, cx));
        }
        cx.update(|_| engine_ended());
    })
    .detach();
}

/// The engine once it runs; until then its start begins (once a window has drawn) on a thread of its own, and the
/// pages wait (module docs).
fn engine() -> Result<Rc<dyn Web>, String> {
    let current = with(|registry| match &registry.engine {
        EngineState::Running(web) => Some(Ok(Rc::clone(web))),
        EngineState::Failed(failure) => Some(Err(failure.clone())),
        EngineState::Starting => Some(Err(STARTING.to_owned())),
        EngineState::Idle => None,
    });
    if let Some(current) = current {
        return current;
    }
    if let Some(failure) = super::sandbox::failure() {
        return Err(failure);
    }
    let caps = gpui_kit::dmabuf_import_caps().ok_or_else(|| NOT_READY.to_owned())?;
    let frames = match negotiate(Some(&caps)) {
        FrameSource::DmaBuf { render_node, formats } => FrameMode::DmaBuf(DmaBufTarget {
            render_node,
            formats: formats
                .into_iter()
                .map(|format| BufferFormat {
                    fourcc: format.fourcc,
                    modifiers: format.modifiers,
                })
                .collect(),
        }),
        // logged by `negotiate`, with the note that video is not supported in this mode
        FrameSource::Shm { .. } => FrameMode::Shm,
    };
    let config = HostConfig {
        frames,
        schemes: vec![GRANT_SCHEME.to_owned()],
    };
    let failed = |failure: String| {
        tracing::error!(failure);
        with(|registry| registry.engine = EngineState::Failed(failure.clone()));
        Err(failure)
    };
    let Some(pump) = with(|registry| registry.pump.clone()) else {
        return failed("the web engine has no event pump (html_overlay::init was not called)".to_owned());
    };
    let spawned = std::thread::Builder::new().name("wpe-start".to_owned()).spawn(move || {
        let _ = pump.send_blocking(ngterm_wpe::Host::start(config));
    });
    match spawned {
        Ok(_) => {
            with(|registry| registry.engine = EngineState::Starting);
            Err(STARTING.to_owned())
        }
        Err(err) => failed(format!("the web engine did not start: {err}")),
    }
}

/// The start thread's outcome, on the main thread: the engine runs and the waiting pages are realized on it, or they
/// fail with its reason. A start that ends after the quit began is let go (its engine shut down off the main thread).
/// The engine's events, when it runs.
fn started_engine(started: Started) -> Option<async_channel::Receiver<Event>> {
    let waited = with(|registry| matches!(registry.engine, EngineState::Starting));
    match started {
        Ok((host, events)) if waited => {
            tracing::info!(backend = ?host.backend(), "the web engine started");
            run_on(Rc::new(Engine(Arc::new(host))));
            Some(events)
        }
        Ok((host, _)) => {
            let _ = std::thread::Builder::new()
                .name("wpe-stop".to_owned())
                .spawn(move || host.shutdown());
            None
        }
        Err(err) if waited => {
            let failure = format!("the web engine did not start: {err}");
            tracing::error!(failure);
            with(|registry| registry.engine = EngineState::Failed(failure));
            // the waiting pages fail with it
            realize_waiting();
            None
        }
        Err(_) => None,
    }
}

/// The started engine `web` runs the pages: the ones waiting for it are realized on it.
fn run_on(web: Rc<dyn Web>) {
    with(|registry| registry.engine = EngineState::Running(web));
    realize_waiting();
}

/// Ends a start [`set_starting`] began with `web` as the engine (the unit tests' recorder).
#[cfg(test)]
pub(super) fn finish_start(web: Rc<dyn Web>) {
    run_on(web);
}

/// The engine's events ended. Past the quit's own shutdown nothing follows; while the app runs it means the engine is
/// gone (its worker died): every page fails, for good, and a store clear in flight reports the failure.
pub(super) fn engine_ended() {
    let pages = with(|registry| {
        if !matches!(registry.engine, EngineState::Running(_)) {
            return None;
        }
        registry.engine = EngineState::Failed(ENDED.to_owned());
        if registry.clearing {
            registry.clearing = false;
            registry.clear_failure = Some(ENDED.to_owned());
        }
        Some(registry.pages.values().cloned().collect::<Vec<_>>())
    });
    let Some(pages) = pages else { return };
    tracing::error!("{ENDED}");
    for page in pages {
        page.engine_failed(ENDED);
    }
}

/// Puts the engine back to starting, its start's outcome then given with [`started_engine`] (the unit tests).
#[cfg(test)]
pub(super) fn set_starting() -> impl FnOnce(Started) {
    with(|registry| registry.engine = EngineState::Starting);
    |started| {
        let _ = started_engine(started);
    }
}

/// Why pages are refused now, as the core is told (the unit tests).
#[cfg(test)]
pub(super) fn refusal() -> Option<String> {
    PageHost.web_views_unavailable()
}

/// Runs the pages against `web` instead of starting the engine (the unit tests' recorder).
#[cfg(test)]
pub(super) fn use_web(web: Rc<dyn Web>) {
    with(|registry| registry.engine = EngineState::Running(web));
}

/// Forgets every page and the engine (between unit tests on one thread).
#[cfg(test)]
pub(super) fn reset() {
    with(|registry| *registry = Registry::default());
}

/// Ends the engine at the quit: every page closes and the engine stops (a bounded wait).
pub fn shutdown() {
    let engine = with(|registry| {
        registry.pages.clear();
        registry.by_view.clear();
        std::mem::replace(
            &mut registry.engine,
            EngineState::Failed("the app is quitting".to_owned()),
        )
    });
    if let EngineState::Running(web) = engine {
        web.shut_down();
    }
}

/// Realizes the pages that wait for the engine; a start that failed fails them.
pub(super) fn realize_waiting() {
    let waiting: Vec<Rc<Page>> = with(|registry| {
        registry
            .pages
            .values()
            .filter(|page| page.view_id().is_none() && page.web().is_none() && page.active())
            .cloned()
            .collect()
    });
    if waiting.is_empty() {
        return;
    }
    match engine() {
        Ok(web) => {
            for page in waiting {
                realize(&page, &web);
            }
        }
        Err(failure) if failure != NOT_READY && failure != STARTING => {
            for page in waiting {
                page.engine_failed(&failure);
            }
        }
        Err(_) => {}
    }
}

/// Makes page `page`'s engine page on `web`, on its store.
fn realize(page: &Rc<Page>, web: &Rc<dyn Web>) {
    let session = match page.storage() {
        Storage::Unavailable(_) => None,
        Storage::Saved => saved_session(web),
        Storage::Ephemeral => {
            let session = web.create_session(SessionSpec::Ephemeral);
            with(|registry| registry.ephemeral.insert(page.id(), session));
            Some(session)
        }
    };
    let zoom = with(|registry| registry.zoom.unwrap_or(1.0));
    page.realize(web, session, zoom);
}

/// Records that page `id`'s engine page is `view`.
pub(super) fn page_view_created(id: Uuid, view: ViewId) {
    with(|registry| registry.by_view.insert(view, id));
}

/// The element of page `overlay`, made with the page on first use and loading (upstream's
/// `page(for:store:backgroundColor:)`); `background` is the overlay's own background, which replaces the theme's.
pub fn mount(
    overlay: &HtmlOverlay,
    background: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<PageView>> {
    let page = page(overlay.id).unwrap_or_else(|| {
        let (colors, poster) = with(|registry| (registry.colors.clone(), registry.poster.clone()));
        let poster = poster.unwrap_or_else(crate::state::detached_poster);
        let page = Page::new(
            overlay,
            background,
            PanelColors::resolve(&colors, background),
            storage(overlay),
            &poster,
        );
        with(|registry| registry.pages.insert(overlay.id, Rc::clone(&page)));
        page
    });
    // a session moved to another window gets an element there: an element's focus is its window's
    if let Some(element) = page
        .element()
        .filter(|_| page.element_window() == Some(window.window_handle()))
    {
        return Some(element);
    }
    let moved = page.element().is_some();
    let element = cx.new(|cx| PageView::new(Rc::clone(&page), window, cx));
    if moved {
        // the new element has no frame yet: the page draws one anew
        page.redraw();
    }
    realize_waiting();
    Some(element)
}

/// Gives page `overlay.id` the model's latest value and whether it is on screen (upstream's `updateNSView`).
pub fn apply(overlay: &HtmlOverlay, on_screen: bool, cx: &mut App) {
    if let Some(page) = page(overlay.id) {
        page.apply(overlay, on_screen, cx);
    }
}

/// After a commit: the pages that left every slot release their engine pages (upstream's `release`), and every page
/// follows the page zoom setting (upstream's `setZoom`).
pub fn settle(core: &mut Core, cx: &mut App) {
    for id in core.take_released_pages() {
        let released = with(|registry| {
            let page = registry.pages.remove(&id)?;
            if let Some(view) = page.view_id() {
                registry.by_view.remove(&view);
            }
            let ephemeral = registry.ephemeral.remove(&id);
            Some((page, ephemeral))
        });
        if let Some((page, ephemeral)) = released {
            page.close(cx);
            if let (Some(session), Some(web)) = (ephemeral, page.web()) {
                web.drop_session(session);
            }
        }
    }
    let zoom = core.html_zoom();
    let changed = with(|registry| {
        let changed = registry.zoom.map(f64::to_bits) != Some(zoom.to_bits());
        registry.zoom = Some(zoom);
        changed.then(|| registry.pages.values().cloned().collect::<Vec<_>>())
    });
    for page in changed.unwrap_or_default() {
        page.set_zoom(zoom);
    }
}

/// Every page follows the terminal's `colors` (upstream's `refreshThemes`, run on `.agtermAppearanceChanged`): its
/// panel is redrawn and a changed theme reaches its scripts. Call it after every config reload or theme change.
pub fn refresh_themes(colors: &TerminalColors, cx: &mut App) {
    let pages = with(|registry| {
        registry.colors = colors.clone();
        registry.pages.values().cloned().collect::<Vec<_>>()
    });
    for page in pages {
        page.apply_theme(colors, cx);
    }
}

/// A history step or hand-off on page `id` (upstream's `navigate`), shared by the strip and the control API.
pub(super) fn navigate(id: Uuid, navigation: HtmlNavigation) -> Result<(), String> {
    let page = page(id).ok_or_else(|| errors::NOT_REALIZED.to_owned())?;
    if page.web().is_none() {
        return Err(errors::NOT_REALIZED.to_owned());
    }
    page.navigate(navigation)
}

/// Puts page `id`'s address on the clipboard (upstream's `copyLink`).
pub(super) fn copy_link(id: Uuid, cx: &mut App) {
    if let Some(link) = page(id).and_then(|page| page.browser_url()) {
        cx.write_to_clipboard(ClipboardItem::new_string(link.as_str().to_owned()));
    }
}

/// Opens `uri` with the default web browser whatever its type, so a file shows as a page rather than going to the app
/// its type maps to (upstream's `SystemBrowser.open`); false when there is no browser.
pub(super) fn open_in_browser(uri: &str) -> bool {
    use gio::prelude::AppInfoExt as _;
    let Some(browser) = gio::AppInfo::default_for_uri_scheme("https") else {
        return false;
    };
    if let Err(err) = browser.launch_uris(&[uri], None::<&gio::AppLaunchContext>) {
        tracing::warn!(%err, "the web browser did not open the page");
    }
    true
}

// MARK: - browser stores

/// Why the saved store cannot be used, `None` when it can, reading the profile on first use (upstream's
/// `persistentStoreFailure`): kept under `<state>/browser/<profile id>/`, data and cache apart.
fn persistent_store_failure() -> Option<String> {
    let (known, clearing, state_dir) = with(|registry| {
        (
            registry.saved_dirs.is_some(),
            registry.clearing,
            registry.state_dir.clone(),
        )
    });
    if clearing {
        return Some(errors::CLEARING.to_owned());
    }
    if known {
        return None;
    }
    let Some(state_dir) = state_dir else {
        return Some(errors::PERSISTENT_UNAVAILABLE.to_owned());
    };
    let id = match BrowserProfile::new(&state_dir).identifier() {
        Ok(id) => id,
        Err(failure) => return Some(failure.to_string()),
    };
    let root = state_dir.join("browser").join(uuid_string(id));
    with(|registry| registry.saved_dirs = Some((root.join("data"), root.join("cache"))));
    None
}

/// The saved store on `web`, made on first use; `None` without its directories.
fn saved_session(web: &Rc<dyn Web>) -> Option<SessionId> {
    let (saved, dirs) = with(|registry| (registry.saved, registry.saved_dirs.clone()));
    if saved.is_some() {
        return saved;
    }
    let (data_dir, cache_dir) = dirs?;
    let session = web.create_session(SessionSpec::Persistent { data_dir, cache_dir });
    with(|registry| registry.saved = Some(session));
    Some(session)
}

/// Starts removing everything the saved store holds (upstream's `clearPersistentStore`): refused while a clear runs,
/// while a page built on the store is still registered (a soft-closed session's included, whose login would be written
/// back), and when the profile cannot be read; a profile never created has nothing to remove and is not created for
/// it. The engine empties the store asynchronously; [`HtmlHost::saved_store_clear`] runs until it is done and then
/// reports the engine's error, if any, once.
fn clear_saved_store() -> Result<StoreClear, String> {
    let (clearing, open, state_dir) = with(|registry| {
        let open = registry.pages.values().filter(|page| page.uses_saved_store()).count();
        (registry.clearing, open, registry.state_dir.clone())
    });
    let profile = state_dir.as_deref().map(BrowserProfile::new);
    if !profile::clear_check(clearing, open, profile.as_ref())? {
        return Ok(StoreClear::Nothing);
    }
    if let Some(failure) = persistent_store_failure() {
        return Err(failure);
    }
    let web = engine().map_err(|_| errors::PERSISTENT_UNAVAILABLE.to_owned())?;
    let session = saved_session(&web).ok_or_else(|| errors::PERSISTENT_UNAVAILABLE.to_owned())?;
    with(|registry| {
        registry.clearing = true;
        registry.clear_failure = None;
    });
    web.clear_session(session);
    Ok(StoreClear::Started)
}

/// The store page `overlay` is built on (upstream's `storage(for:)`): a persistent URL page the saved one, or none
/// when it cannot be used, since an in-memory one would hold a login the user asked to keep; every other page an
/// in-memory one of its own.
fn storage(overlay: &HtmlOverlay) -> Storage {
    if !(overlay.flags.persistent && matches!(overlay.source, HtmlSource::Url(_))) {
        return Storage::Ephemeral;
    }
    match persistent_store_failure() {
        Some(failure) => Storage::Unavailable(failure),
        None => Storage::Saved,
    }
}

// MARK: - the engine's events

/// One of the engine's events, on the main thread.
pub(super) fn dispatch(event: &Event, cx: &mut App) {
    match event {
        Event::Frame(frame) => to_element(frame.view, event, cx),
        Event::BuffersRemoved { view, .. } | Event::Cursor { view, .. } | Event::ImeCursorArea { view, .. } => {
            to_element(*view, event, cx);
        }
        Event::OptionMenu(menu) => to_element(menu.view, event, cx),
        Event::ContextMenu(menu) => to_element(menu.view, event, cx),
        Event::OptionMenuClosed { token } => option_menu_closed(*token, cx),
        Event::Load { view, .. }
        | Event::LoadFailed { view, .. }
        | Event::WebProcessTerminated { view, .. }
        | Event::Page { view, .. }
        | Event::CloseRequested { view }
        | Event::ViewFailed { view, .. } => to_page(*view, event, cx),
        Event::Policy(request) => match page_of_view(request.view) {
            Some(page) => page.event(event, cx),
            None => answer_with(|web| web.decide_policy(request.token, false)),
        },
        Event::ScriptMessage(message) => match page_of_view(message.view) {
            Some(page) => page.event(event, cx),
            None => answer_with(|web| web.reply_script_message(message.token, Err("page closed".to_owned()))),
        },
        Event::SchemeRequest(request) => {
            let root = request.view.and_then(page_of_view).and_then(|page| page.grant_root());
            let response = if request.view.is_some() {
                policy::serve_granted(root.as_deref(), &request.uri)
            } else {
                SchemeResponse::Error {
                    kind: SchemeError::PermissionDenied,
                    message: policy::OUTSIDE_GRANT.to_owned(),
                }
            };
            answer_with(|web| web.finish_scheme_request(request.token, response));
        }
        Event::Clipboard { text, html } => write_clipboard(text, html.as_deref(), cx),
        Event::DownloadRefused { uri, .. } => tracing::info!(uri, "a page's download was refused"),
        Event::SessionCleared { result, .. } => with(|registry| {
            registry.clearing = false;
            registry.clear_failure = result.as_ref().err().cloned();
            if let Err(err) = result {
                tracing::warn!(err, "the web engine did not empty the saved browser store");
            }
        }),
    }
}

fn answer_with(answer: impl FnOnce(&dyn Web)) {
    let web = with(|registry| match &registry.engine {
        EngineState::Running(web) => Some(Rc::clone(web)),
        _ => None,
    });
    if let Some(web) = web {
        answer(web.as_ref());
    }
}

fn to_page(view: ViewId, event: &Event, cx: &mut App) {
    if let Some(page) = page_of_view(view) {
        page.event(event, cx);
    }
}

fn to_element(view: ViewId, event: &Event, cx: &mut App) {
    if let Some(element) = page_of_view(view).and_then(|page| page.element()) {
        element.update(cx, |element, cx| element.event(event, cx));
        return;
    }
    // a frame nobody shows still frees its buffer, a popup nobody shows closes
    if let Event::Frame(frame) = event {
        answer_with(|web| web.presenter()(frame.view, frame.buffer));
    }
    if let Event::OptionMenu(menu) = event {
        answer_with(|web| web.choose_option(menu.token, None));
    }
}

fn option_menu_closed(token: ngterm_wpe::MenuToken, cx: &mut App) {
    let elements: Vec<Entity<PageView>> =
        with(|registry| registry.pages.values().filter_map(|page| page.element()).collect());
    for element in elements {
        element.update(cx, |element, cx| element.menu_closed(token, cx));
    }
}

/// A page's copy reaches the desktop's clipboard, with its HTML where the platform offers it.
fn write_clipboard(text: &str, html: Option<&str>, cx: &mut App) {
    let item = ClipboardItem::new_string(text.to_owned());
    #[cfg(target_os = "linux")]
    if let Some(html) = html {
        let offer = gpui_linux::ClipboardHtml {
            html: html.to_owned(),
            with_text: true,
        };
        gpui_linux::with_clipboard_html(offer, || cx.write_to_clipboard(item));
        return;
    }
    let _ = html;
    cx.write_to_clipboard(item);
}

// MARK: - page requests

/// Keeps a page request's reply until its answer; the token names it.
pub(super) fn park(reply: ReplyToken) -> u64 {
    with(|registry| {
        registry.next_reply += 1;
        let token = registry.next_reply;
        registry.replies.insert(token, reply);
        token
    })
}

/// Answers the page request `token` with `response` (upstream's `HtmlOverlayBridge.reply`): its result as a JSON value,
/// or its error for a refused request.
pub fn answer(token: u64, response: &Response) {
    finish(token, bridge::reply(response));
}

/// Rejects the page request `token` with `message`.
pub fn reject(token: u64, message: &str) {
    finish(token, Err(message.to_owned()));
}

fn finish(token: u64, answer: Result<serde_json::Value, String>) {
    let Some(reply) = with(|registry| registry.replies.remove(&token)) else {
        return;
    };
    answer_with(|web| web.reply_script_message(reply, answer.map(|value| value.to_string())));
}

// MARK: - the keyboard

/// A key the app's interceptor hands on: the page holding `window`'s keyboard takes it (`PageView::key_down`).
/// Whether one did.
pub fn key_down(
    keystroke: &gpui_kit::Keystroke,
    stroke: &crate::terminal::input::KeyStroke,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let elements: Vec<Entity<PageView>> =
        with(|registry| registry.pages.values().filter_map(|page| page.element()).collect());
    let Some(focused) = elements
        .into_iter()
        .find(|element| element.read(cx).focus().is_focused(window))
    else {
        return false;
    };
    let source = crate::terminal::input::current_source(keystroke);
    focused.update(cx, |element, cx| element.key_down(keystroke, stroke, &source, cx))
}
