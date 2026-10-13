//! The pages by id, the browser stores they are built on, the grant's URI scheme and the page requests waiting on an
//! answer (upstream's `HtmlOverlayRegistry`). It lives on the GTK thread; every borrow is short and never spans a
//! `WebKit` call, since `WebKit` may call back into a page's handlers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::html::bridge;
use agterm_core::html::profile::BrowserProfile;
use agterm_core::html::{HtmlHost, HtmlNavigation, HtmlOverlay, HtmlSource, errors};
use agterm_core::pane_env::uuid_string;
use agterm_proto::Response;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use uuid::Uuid;
use webkit6::{NetworkSession, ScriptMessageReply, URISchemeRequest, WebContext, WebView, javascriptcore};

use super::page::{Page, Storage};
use super::policy::{self, GRANT_SCHEME};
use crate::state::Poster;

/// A page request's reply, kept until the request is answered.
struct Parked {
    reply: ScriptMessageReply,
    context: javascriptcore::Context,
}

#[derive(Default)]
struct Registry {
    pages: HashMap<Uuid, Rc<Page>>,
    /// The page zoom every page shows at; `None` before the first settle (actual size).
    zoom: Option<f64>,
    /// Where the browser profile and the saved store live.
    state_dir: Option<PathBuf>,
    /// The saved browser store, built on first use (upstream's `persistentStore`).
    saved: Option<NetworkSession>,
    replies: HashMap<u64, Parked>,
    next_reply: u64,
    scheme_registered: bool,
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

/// The core's way to the web views (upstream's registry calls in `ControlServer+SessionActions.swift`).
struct PageHost;

impl HtmlHost for PageHost {
    fn persistent_store_failure(&self) -> Option<String> {
        persistent_store_failure()
    }

    fn navigate(&self, id: Uuid, navigation: HtmlNavigation) -> Result<(), String> {
        navigate(id, navigation)
    }
}

/// Makes the registry the core's page host, its saved store kept under `state_dir` (upstream's `install` and
/// `profile`).
pub fn install(core: &mut Core, state_dir: PathBuf) {
    with(|registry| registry.state_dir = Some(state_dir));
    core.set_html_host(Box::new(PageHost));
}

/// The panel of page `overlay`, created and loading on first use (upstream's `page(for:store:backgroundColor:)`);
/// `background` is the overlay's own background, which replaces the theme's.
#[must_use]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature the build without WebKitGTK shares, which never has a panel"
)]
pub fn mount(overlay: &HtmlOverlay, background: Option<&str>, poster: &Poster) -> Option<gtk::Widget> {
    if let Some(page) = page(overlay.id) {
        return Some(page.widget());
    }
    register_scheme();
    let storage = storage(overlay);
    let zoom = with(|registry| registry.zoom.unwrap_or(1.0));
    let page = Page::new(overlay, background, poster, storage, zoom);
    let widget = page.widget();
    with(|registry| registry.pages.insert(overlay.id, page));
    Some(widget)
}

/// Gives page `overlay.id` the model's latest value and whether it is on screen (upstream's `updateNSView`).
pub fn apply(overlay: &HtmlOverlay, on_screen: bool) {
    if let Some(page) = page(overlay.id) {
        page.apply(overlay, on_screen);
    }
}

/// The widget that takes the keyboard for page `id`: its web view.
#[must_use]
pub fn focus_target(id: Uuid) -> Option<gtk::Widget> {
    page(id).map(|page| page.web_view().clone().upcast())
}

/// Whether page `id`'s web view is its window's focus widget now.
pub(super) fn holds_focus(id: Uuid) -> bool {
    page(id).is_some_and(|page| page.web_view().is_focus())
}

/// After a commit: the pages that left every slot release their web views (upstream's `release`), and every page
/// follows the page zoom setting (upstream's `setZoom`).
pub fn settle(core: &mut Core) {
    for id in core.take_released_pages() {
        if let Some(page) = with(|registry| registry.pages.remove(&id)) {
            page.close();
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

/// A history step or hand-off on page `id` (upstream's `navigate`), shared by the toolbar and the control API.
pub(super) fn navigate(id: Uuid, navigation: HtmlNavigation) -> Result<(), String> {
    page(id)
        .ok_or_else(|| errors::NOT_REALIZED.to_owned())?
        .navigate(navigation)
}

/// Puts page `id`'s address on the clipboard (upstream's `copyLink`).
pub(super) fn copy_link(id: Uuid) {
    let Some(link) = page(id).and_then(|page| page.browser_url()) else {
        return;
    };
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(link.as_str());
    }
}

/// Opens `uri` with the default web browser whatever its type, so a file shows as a page rather than going to the app
/// its type maps to (upstream's `SystemBrowser.open`); false when there is no browser.
pub(super) fn open_in_browser(uri: &str) -> bool {
    let Some(browser) = gio::AppInfo::default_for_uri_scheme("https") else {
        return false;
    };
    if let Err(err) = browser.launch_uris(&[uri], None::<&gio::AppLaunchContext>) {
        tracing::warn!(%err, "the web browser did not open the page");
    }
    true
}

// MARK: - browser stores

/// Why the saved store cannot be used, `None` when it can, building it on first use (upstream's
/// `persistentStoreFailure`): kept under `<state>/browser/<profile id>/`, data and cache apart.
fn persistent_store_failure() -> Option<String> {
    let (built, state_dir) = with(|registry| (registry.saved.is_some(), registry.state_dir.clone()));
    if built {
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
    let data = root.join("data");
    let cache = root.join("cache");
    let session = NetworkSession::new(Some(&data.to_string_lossy()), Some(&cache.to_string_lossy()));
    refuse_downloads(&session);
    with(|registry| registry.saved = Some(session));
    None
}

/// The store page `overlay` is built on (upstream's `storage(for:)`): a persistent URL page the saved one, or none
/// when it cannot be used, since an in-memory one would hold a login the user asked to keep; every other page an
/// in-memory one of its own.
fn storage(overlay: &HtmlOverlay) -> Storage {
    if !(overlay.flags.persistent && matches!(overlay.source, HtmlSource::Url(_))) {
        let session = NetworkSession::new_ephemeral();
        refuse_downloads(&session);
        return Storage::Ready(session);
    }
    if let Some(failure) = persistent_store_failure() {
        return Storage::Unavailable(failure);
    }
    with(|registry| registry.saved.clone()).map_or_else(
        || Storage::Unavailable(errors::PERSISTENT_UNAVAILABLE.to_owned()),
        Storage::Ready,
    )
}

/// Nothing a page shows is saved to disk.
fn refuse_downloads(session: &NetworkSession) {
    session.connect_download_started(|_, download| download.cancel());
}

// MARK: - the grant's scheme

/// Registers [`GRANT_SCHEME`] on the default web context once.
fn register_scheme() {
    if with(|registry| std::mem::replace(&mut registry.scheme_registered, true)) {
        return;
    }
    if let Some(context) = WebContext::default() {
        context.register_uri_scheme(GRANT_SCHEME, serve);
    } else {
        tracing::warn!("no web context: granted file pages cannot load");
    }
}

/// The grant root of the page `view` shows.
fn grant_root_of(view: &WebView) -> Option<PathBuf> {
    let page = with(|registry| registry.pages.values().find(|page| page.web_view() == view).cloned())?;
    page.grant_root()
}

/// Serves one read of a granted page: the file when it sits inside the page's grant, else an error.
fn serve(request: &URISchemeRequest) {
    let root = request.web_view().and_then(|view| grant_root_of(&view));
    let requested = request
        .uri()
        .and_then(|uri| url::Url::parse(&policy::app_uri(&uri)).ok())
        .and_then(|url| url.to_file_path().ok());
    let resolved = match policy::granted_file(root.as_deref(), requested.as_deref()) {
        Ok(resolved) => resolved,
        Err(message) => {
            let code = if message == policy::NOT_A_FILE {
                gio::IOErrorEnum::IsDirectory
            } else {
                gio::IOErrorEnum::PermissionDenied
            };
            request.finish_error(&mut glib::Error::new(code, message));
            return;
        }
    };
    match gio::File::for_path(&resolved).read(None::<&gio::Cancellable>) {
        Ok(stream) => {
            let length = std::fs::metadata(&resolved)
                .ok()
                .and_then(|metadata| i64::try_from(metadata.len()).ok())
                .unwrap_or(-1);
            let (content_type, _) = gio::content_type_guess(Some(&resolved), None::<&[u8]>);
            let mime = gio::content_type_get_mime_type(&content_type);
            request.finish(
                &stream,
                length,
                Some(mime.as_deref().unwrap_or("application/octet-stream")),
            );
        }
        Err(mut error) => request.finish_error(&mut error),
    }
}

// MARK: - page requests

/// Keeps a page request's reply until its answer; the token names it.
pub(super) fn park(reply: ScriptMessageReply, context: javascriptcore::Context) -> u64 {
    with(|registry| {
        registry.next_reply += 1;
        let token = registry.next_reply;
        registry.replies.insert(token, Parked { reply, context });
        token
    })
}

/// Answers the page request `token` with `response` (upstream's `HtmlOverlayBridge.reply`): its result as a JSON value,
/// or its error for a refused request.
pub fn answer(token: u64, response: &Response) {
    finish(token, bridge::reply(response));
}

/// Rejects the page request `token` with `message`.
pub(super) fn reject(token: u64, message: &str) {
    finish(token, Err(message.to_owned()));
}

fn finish(token: u64, answer: Result<serde_json::Value, String>) {
    let Some(parked) = with(|registry| registry.replies.remove(&token)) else {
        return;
    };
    match answer {
        Ok(value) => {
            let value = javascriptcore::Value::from_json(&parked.context, &value.to_string());
            parked.reply.return_value(&value);
        }
        Err(message) => parked.reply.return_error_message(&message),
    }
}
