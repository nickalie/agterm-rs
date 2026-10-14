//! The pages by id, the browser stores they are built on, the grant's URI scheme and the page requests waiting on an
//! answer (upstream's `HtmlOverlayRegistry`). It lives on the GTK thread; every borrow is short and never spans a
//! `WebKit` call, since `WebKit` may call back into a page's handlers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use agterm_core::Core;
use agterm_core::html::bridge;
use agterm_core::html::profile::{self, BrowserProfile};
use agterm_core::html::{ClearProgress, HtmlHost, HtmlNavigation, HtmlOverlay, HtmlSource, StoreClear, errors};
use agterm_core::pane_env::uuid_string;
use agterm_proto::Response;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use uuid::Uuid;
use webkit6::{
    CookiePersistentStorage, NetworkSession, ScriptMessageReply, URISchemeRequest, WebContext, WebView,
    WebsiteDataTypes, javascriptcore,
};

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
    /// The web engine is emptying the saved store (upstream's `clearing`): no page may use it meanwhile.
    clearing: bool,
    /// Why the web engine could not empty the saved store, kept for the poll that answers the clear.
    clear_failure: Option<String>,
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
    fn web_views_unavailable(&self) -> Option<String> {
        super::sandbox::failure()
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

/// Whether page `id` is marked on screen; `None` without such a page.
#[cfg(test)]
pub(crate) fn on_screen(id: Uuid) -> Option<bool> {
    page(id).map(|page| page.on_screen())
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

/// Every page follows the terminal's current colors (upstream's `refreshThemes`, run on `.agtermAppearanceChanged`):
/// its panel is redrawn and a changed theme reaches its scripts. Call it on the GTK thread after every config reload
/// or theme change.
pub fn refresh_themes() {
    let pages = with(|registry| registry.pages.values().cloned().collect::<Vec<_>>());
    for page in pages {
        page.apply_theme();
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
    let (built, clearing, state_dir) =
        with(|registry| (registry.saved.is_some(), registry.clearing, registry.state_dir.clone()));
    if clearing {
        return Some(errors::CLEARING.to_owned());
    }
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
    let session = saved_session(&data, &cache);
    with(|registry| registry.saved = Some(session));
    None
}

/// A store kept under `data` and `cache`, cookies included (upstream's `WKWebsiteDataStore(forIdentifier:)`):
/// `WebKit` keeps a session's cookies in memory unless told where to save them, so they go to `cookies.sqlite` in the
/// data directory, which a later session on the same directories reads back.
fn saved_session(data: &Path, cache: &Path) -> NetworkSession {
    let session = NetworkSession::new(Some(&data.to_string_lossy()), Some(&cache.to_string_lossy()));
    if let Some(cookies) = session.cookie_manager() {
        cookies.set_persistent_storage(
            &data.join("cookies.sqlite").to_string_lossy(),
            CookiePersistentStorage::Sqlite,
        );
    } else {
        tracing::warn!("the saved browser store has no cookie manager: its logins last until agterm quits");
    }
    refuse_downloads(&session);
    session
}

/// Starts removing everything the saved store holds (upstream's `clearPersistentStore`): refused while a clear runs,
/// while a page built on the store is still registered (a soft-closed session's included, whose login would be written
/// back), and when the profile cannot be read; a profile never created has nothing to remove and is not created for
/// it. The web engine empties the store asynchronously; [`HtmlHost::saved_store_clear`] runs until it is done and
/// then reports the engine's error, if any, once.
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
    let Some(manager) = with(|registry| registry.saved.as_ref().and_then(NetworkSession::website_data_manager)) else {
        return Err(errors::PERSISTENT_UNAVAILABLE.to_owned());
    };
    with(|registry| {
        registry.clearing = true;
        registry.clear_failure = None;
    });
    manager.clear(
        WebsiteDataTypes::ALL,
        glib::TimeSpan::from_seconds(0),
        None::<&gio::Cancellable>,
        |cleared| {
            let failure = cleared.err().map(|err| {
                tracing::warn!(%err, "the web engine did not empty the saved browser store");
                err.to_string()
            });
            with(|registry| {
                registry.clearing = false;
                registry.clear_failure = failure;
            });
        },
    );
    Ok(StoreClear::Started)
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

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use webkit6::prelude::WebViewExt;

    use super::*;

    /// A local web server: `/set` sets a lasting login cookie, any other path titles its page with the cookies the
    /// request carried (`cookie=<header>`).
    struct Server {
        origin: String,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Server {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = Arc::clone(&stop);
            let thread = std::thread::spawn(move || serve_until(&listener, &stopping));
            Self {
                origin,
                stop,
                thread: Some(thread),
            }
        }
    }

    /// Answers every connection `listener` takes until `stop` is set.
    fn serve_until(listener: &TcpListener, stop: &AtomicBool) {
        for stream in listener.incoming() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(stream) = stream {
                respond(stream);
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // wakes the accept loop so it sees the stop
            let _ = TcpStream::connect(self.origin.trim_start_matches("http://"));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Answers one request, closing the connection after.
    fn respond(mut stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut path = String::new();
        let mut cookie = String::new();
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
            let header = line.trim_end();
            if header.is_empty() {
                break;
            }
            if path.is_empty() {
                path = header.split(' ').nth(1).unwrap_or_default().to_owned();
            } else if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("cookie")
            {
                value.trim().clone_into(&mut cookie);
            }
            line.clear();
        }
        let (set_cookie, title) = if path == "/set" {
            ("Set-Cookie: login=kept; Max-Age=3600; Path=/\r\n", "set".to_owned())
        } else {
            ("", format!("cookie={cookie}"))
        };
        let body = format!("<html><head><title>{title}</title></head><body></body></html>");
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{set_cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    }

    /// Loads `uri` in a web view on `session` and hands back its title once `done` accepts it, or the last one after
    /// ten seconds; the web view is closed after.
    fn title_of(session: &NetworkSession, uri: &str, done: impl Fn(&str) -> bool) -> String {
        let view = WebView::builder().network_session(session).build();
        view.load_uri(uri);
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut title = String::new();
        while Instant::now() < deadline {
            while context.iteration(false) {}
            title = view.title().map(Into::into).unwrap_or_default();
            if done(&title) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        view.try_close();
        title
    }

    fn cookie_title(title: &str) -> bool {
        title.starts_with("cookie=")
    }

    #[test]
    fn a_saved_stores_login_outlives_its_session() {
        crate::gtk_test::on_gtk_thread(saved_login_outlives_its_session);
    }

    /// A login cookie a page sets in the saved store is read back by a store built anew on the same profile, as after
    /// a restart; a page in an in-memory store never sees it.
    fn saved_login_outlives_its_session() {
        let state = tempfile::tempdir().unwrap();
        let server = Server::start();
        let get = format!("{}/get", server.origin);
        let saved = || {
            with(|registry| {
                registry.state_dir = Some(state.path().to_owned());
                registry.saved = None;
            });
            assert_eq!(persistent_store_failure(), None);
            with(|registry| registry.saved.take()).unwrap()
        };

        let first = saved();
        let set = format!("{}/set", server.origin);
        assert_eq!(title_of(&first, &set, |title| title == "set"), "set");
        assert_eq!(title_of(&first, &get, cookie_title), "cookie=login=kept");
        drop(first);

        let memory = NetworkSession::new_ephemeral();
        assert_eq!(title_of(&memory, &get, cookie_title), "cookie=");

        let reopened = saved();
        assert_eq!(
            title_of(&reopened, &get, cookie_title),
            "cookie=login=kept",
            "a store built anew on the same profile keeps the login"
        );
        drop(reopened);
        with(|registry| registry.state_dir = None);
    }
}
