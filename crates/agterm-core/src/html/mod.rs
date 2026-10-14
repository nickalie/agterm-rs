//! HTML overlays: a page occupying an overlay slot in place of a program (upstream's `HtmlOverlay.swift`,
//! `Session+HtmlOverlay.swift`): what a page shows ([`HtmlSource`]), its origin, the page-zoom ladder, the page value
//! the model carries ([`HtmlOverlay`]), the checks `session.overlay.open` runs before any web view exists (`grantError`,
//! `webURL`), the theme a page sees, and the seam to the frontend's web views ([`HtmlHost`]). The navigation policy,
//! the page bridge, the outcomes and the browser profile have modules of their own.

pub mod bridge;
pub mod navigation;
pub mod outcomes;
pub mod profile;
pub mod theme;

#[cfg(test)]
mod page_tests;

pub use navigation::{HtmlNavigationAction, HtmlNavigationDecision, HtmlNavigationTarget, HtmlOrigin, decide};
pub use outcomes::HtmlPageOutcomes;
pub use theme::HtmlOverlayTheme;

use url::Url;

/// A page's `--html` file must be absolute (upstream's `grantError` messages, without the command prefix).
pub const FILE_NOT_ABSOLUTE: &str = "html file must be an absolute path";
pub const CWD_NOT_ABSOLUTE: &str = "cwd must be an absolute path";
pub const CWD_ROOT_OR_HOME: &str = "cwd must not be / or the home directory";
pub const CWD_IS_FILE: &str = "cwd must be a directory containing the html file";
pub const OUTSIDE_CWD: &str = "html file is outside cwd";

/// Why a file page and its `--cwd` grant cannot be shown (upstream's `HtmlOverlay.grantError`): the file must be
/// absolute; a grant must be absolute, neither `/` nor `home`, and a directory holding the file. Paths compare by
/// their standardized components, `.` and `..` resolved lexically, never through the file system.
#[must_use]
pub fn grant_error(file: &str, grant_root: Option<&str>, home: &str) -> Option<&'static str> {
    if !file.starts_with('/') {
        return Some(FILE_NOT_ABSOLUTE);
    }
    let grant_root = grant_root?;
    if !grant_root.starts_with('/') {
        return Some(CWD_NOT_ABSOLUTE);
    }
    let root = components(grant_root);
    if root == components("/") || root == components(home) {
        return Some(CWD_ROOT_OR_HOME);
    }
    let file = components(file);
    if root == file {
        return Some(CWD_IS_FILE);
    }
    (!file.starts_with(&root)).then_some(OUTSIDE_CWD)
}

/// Whether `path` lies inside `root`, by standardized components (upstream's `HtmlOverlay.contains`).
#[must_use]
pub fn contains(root: &str, path: &str) -> bool {
    components(path).starts_with(&components(root))
}

/// The components of an absolute path as Foundation's `standardizedFileURL.pathComponents` gives them: the root, then
/// each name, with `.` dropped, `..` taking the previous name off, and empty names (repeated or trailing slashes)
/// ignored.
fn components(path: &str) -> Vec<&str> {
    let mut parts = vec!["/"];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            name => parts.push(name),
        }
    }
    parts
}

/// The URL of an absolute http or https address with a host, `None` for anything else (upstream's
/// `HtmlSource.webURL`, which accepts a URL only when it has a web origin).
#[must_use]
pub fn web_url(text: &str) -> Option<Url> {
    // the URL standard the parser follows repairs `http:host`, `http:/host` and surrounding spaces into an address;
    // Foundation's `URL(string:)` finds no host there, so upstream refuses them
    let (_, rest) = text.split_once(':')?;
    if !rest.starts_with("//") || text.trim() != text {
        return None;
    }
    let url = Url::parse(text).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    (web && url.host_str().is_some_and(|host| !host.is_empty())).then_some(url)
}

/// Where `session.overlay.navigate` takes a page (upstream's `HtmlNavigation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlNavigation {
    Back,
    Forward,
    /// Open the page in the default web browser.
    Browser,
    /// Show a file page in the file manager.
    Finder,
}

impl HtmlNavigation {
    /// Parses the wire spelling.
    #[must_use]
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "back" => Some(Self::Back),
            "forward" => Some(Self::Forward),
            "browser" => Some(Self::Browser),
            "finder" => Some(Self::Finder),
            _ => None,
        }
    }

    /// The wire spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Forward => "forward",
            Self::Browser => "browser",
            Self::Finder => "finder",
        }
    }
}

/// What a page overlay shows (upstream's `HtmlSource`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlSource {
    /// An absolute file. `grant_root` is the directory the web view may read from and the page's base; `None` grants
    /// no file access, so the frontend loads the file's text instead (a single-file grant would widen to its folder).
    File { path: String, grant_root: Option<String> },
    /// A web page, pinned to its origin by the navigation policy.
    Url(Url),
}

impl HtmlSource {
    /// The origin of an HTTP(S) URL without credentials, `None` for other schemes (upstream's `HtmlSource.origin`).
    #[must_use]
    pub fn origin_of(url: &Url) -> Option<String> {
        HtmlOrigin::new(url).map(|origin| origin.display())
    }

    /// What the source names, as the read-back and the strip's tooltip spell it.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::File { path, .. } => path.clone(),
            Self::Url(url) => url.as_str().to_owned(),
        }
    }
}

/// The page-zoom ladder the font-size commands walk on an HTML page, one factor for every page (upstream's
/// `HtmlZoom`).
pub mod zoom {
    pub const STEPS: [f64; 11] = [0.5, 0.75, 0.85, 1.0, 1.15, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
    pub const SMALLEST: f64 = 0.5;
    pub const LARGEST: f64 = 3.0;

    /// A stored factor bounded to the ladder's range; `None` and a non-finite value mean actual size.
    #[must_use]
    pub fn resolve(stored: Option<f64>) -> f64 {
        stored
            .filter(|value| value.is_finite())
            .map_or(1.0, |value| value.clamp(SMALLEST, LARGEST))
    }

    /// The next factor from `zoom` for a font binding action (`increase_font_size:1`, `decrease_font_size:1`,
    /// `reset_font_size`), `None` for any other action. An off-ladder factor steps to its neighbor.
    #[must_use]
    pub fn applying(action: &str, zoom: f64) -> Option<f64> {
        // Swift's `split(separator:)` drops empty pieces
        match action.split(':').find(|piece| !piece.is_empty()) {
            Some("increase_font_size") => Some(STEPS.iter().copied().find(|step| *step > zoom).unwrap_or(LARGEST)),
            Some("decrease_font_size") => Some(
                STEPS
                    .iter()
                    .copied()
                    .rev()
                    .find(|step| *step < zoom)
                    .unwrap_or(SMALLEST),
            ),
            Some("reset_font_size") => Some(1.0),
            _ => None,
        }
    }
}

/// The page's load progress as the frontend's web view last reported it (upstream's `HtmlLoadState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HtmlLoadState {
    #[default]
    Loading,
    Loaded,
    Failed,
}

impl HtmlLoadState {
    /// The wire spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Loaded => "loaded",
            Self::Failed => "failed",
        }
    }
}

/// What a reload loads: the original source (`overlay.reload`, the artifact an agent rewrote) or the page the user
/// navigated to (`--current`, the toolbar button) (upstream's `HtmlReloadTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HtmlReloadTarget {
    #[default]
    Original,
    Current,
}

/// The page the web view shows and where its history can go; `page` differs from the source after a navigation
/// (upstream's `HtmlPageInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlPageInfo {
    /// A file page's path, else the address.
    pub page: String,
    pub title: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// How a page was opened: its `session.overlay.open` flags (upstream's `HtmlOverlay` init arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "upstream's five independent page flags, each its own wire boolean"
)]
pub struct HtmlPageFlags {
    /// History, reload, browser and source-sharing buttons in the identity strip.
    pub navigation: bool,
    /// The page runs its own scripts; agterm's theme script runs either way.
    pub javascript: bool,
    /// No identity strip, file pages only: the strip is what names a URL page's origin.
    pub chromeless: bool,
    /// A URL page on the saved browser store; every other page gets an in-memory one of its own.
    pub persistent: bool,
    /// A URL page's main frame may leave its first origin for any HTTP(S) site.
    pub browse: bool,
}

/// A page occupying an overlay slot (`session.overlay.open --html` or `--url`) in place of a program (upstream's
/// `HtmlOverlay`). `id` travels with the value, so a pane swap or promotion moves the page and the frontend's web view
/// follows it; every frontend report addresses the page by that id. Ephemeral: never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlOverlay {
    pub id: uuid::Uuid,
    pub source: HtmlSource,
    pub flags: HtmlPageFlags,
    pub load_state: HtmlLoadState,
    pub load_error: Option<String>,
    /// What the web view shows now, as last reported; `None` until the first load finishes and after a reload of the
    /// original.
    pub current: Option<HtmlPageInfo>,
    /// Bumped by every reload; the frontend reloads what `reload_target` names when it moves.
    pub reload_revision: u64,
    pub reload_target: HtmlReloadTarget,
}

impl HtmlOverlay {
    /// A loading page with a fresh id.
    #[must_use]
    pub fn new(source: HtmlSource, flags: HtmlPageFlags) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            source,
            flags,
            load_state: HtmlLoadState::Loading,
            load_error: None,
            current: None,
            reload_revision: 0,
            reload_target: HtmlReloadTarget::Original,
        }
    }

    /// Whether agterm themes the page and gives it the bridge: a file page (upstream's `themed`).
    #[must_use]
    pub fn themed(&self) -> bool {
        matches!(self.source, HtmlSource::File { .. })
    }

    /// Whether the page is loaded from its file's text: a file page without a grant, which sits at `about:blank`
    /// and cannot navigate (upstream's `textLoaded`).
    #[must_use]
    pub fn text_loaded(&self) -> bool {
        matches!(self.source, HtmlSource::File { grant_root: None, .. })
    }

    /// What the panel names the page by, from what the app loaded and never from the page's title, which the page
    /// writes (upstream's `identity`): the file shown, the origin a URL page is confined to, or for a browsing page
    /// the origin of the document it shows now (an address with no web origin, `about:blank`, by that address).
    #[must_use]
    pub fn identity(&self) -> String {
        match &self.source {
            HtmlSource::File { path, .. } => {
                let shown = self
                    .current
                    .as_ref()
                    .map_or(path.as_str(), |current| current.page.as_str());
                last_path_component(shown)
            }
            HtmlSource::Url(url) => {
                let shown = self.current.as_ref().filter(|_| self.flags.browse).map(|current| {
                    Url::parse(&current.page)
                        .ok()
                        .and_then(|url| HtmlSource::origin_of(&url))
                        .unwrap_or_else(|| current.page.clone())
                });
                shown
                    .or_else(|| HtmlSource::origin_of(url))
                    .unwrap_or_else(|| url.as_str().to_owned())
            }
        }
    }
}

/// The last component of a path as Foundation's `lastPathComponent` gives it: trailing slashes ignored, `/` for the
/// root.
fn last_path_component(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { String::new() } else { "/".to_owned() };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_owned()
}

/// The frontend's web views, as the dispatcher reaches them (upstream's `HtmlOverlayRegistry` calls in
/// `ControlServer+SessionActions.swift`). A build without one refuses pages.
pub trait HtmlHost {
    /// Why no web view can run here, `None` when one can; asked before any page is accepted, so a web engine that
    /// cannot start is refused to the caller rather than met when the page mounts. Upstream has no such case: its web
    /// views always run.
    fn web_views_unavailable(&self) -> Option<String> {
        None
    }

    /// Why a persistent page cannot get the saved browser store, `None` when it can (upstream's
    /// `persistentStoreFailure`); asked before a persistent page is accepted, so the refusal reaches the caller.
    fn persistent_store_failure(&self) -> Option<String>;

    /// Performs a history step or hand-off on page `page`'s web view (upstream's `HtmlOverlayRegistry.navigate`).
    ///
    /// # Errors
    ///
    /// The control error text ([`errors`]): the page has no web view yet, no history that way, no browser, or a
    /// file-manager hand-off of a URL page.
    fn navigate(&self, page: uuid::Uuid, navigation: HtmlNavigation) -> Result<(), String>;

    /// Starts removing everything the saved browser store holds (upstream's `clearPersistentStore`, its refusals in
    /// the order [`profile::clear_check`] gives them): [`StoreClear::Nothing`] when no store was ever created,
    /// [`StoreClear::Started`] while the web engine empties it, until [`HtmlHost::saved_store_clear`] stops answering
    /// [`ClearProgress::Running`].
    ///
    /// # Errors
    ///
    /// Why the store cannot be cleared now, the text `browser.clear` reports after its own name.
    fn clear_saved_store(&self) -> Result<StoreClear, String> {
        Err(errors::PERSISTENT_UNAVAILABLE.to_owned())
    }

    /// How the clear [`HtmlHost::clear_saved_store`] started is going; a failure is answered once.
    fn saved_store_clear(&self) -> ClearProgress {
        ClearProgress::Done
    }
}

/// How a clear of the saved browser store is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClearProgress {
    /// The web engine is still emptying the store.
    Running,
    /// The store is empty.
    Done,
    /// The web engine reported it could not empty the store: its error text, which `browser.clear` reports after its
    /// own name (upstream's `removeData` cannot fail; the web engine's clear can).
    Failed(String),
}

/// What [`HtmlHost::clear_saved_store`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreClear {
    /// No saved store was ever created: nothing to remove, and none is created for it.
    Nothing,
    /// The web engine is emptying the store.
    Started,
}

/// The page refusals only the frontend's host can tell (upstream's `OverlayHtmlError`).
pub mod errors {
    use super::HtmlNavigation;

    /// The page has not been shown yet, so there is no web view to drive.
    pub const NOT_REALIZED: &str = "html overlay not realized";
    pub const NO_BROWSER: &str = "no default web browser to open the page in";
    pub const FINDER_REQUIRES_FILE: &str = "show in Finder requires a file page";
    pub const PERSISTENT_UNAVAILABLE: &str = "persistent browser storage is not available";
    /// The saved store is being emptied: neither a second clear nor a persistent page may use it (upstream's
    /// `BrowserClearError.clearing`).
    pub const CLEARING: &str = "browser storage is being cleared";

    /// Persistent pages still hold the saved store and would write their logins back (upstream's
    /// `BrowserClearError.pagesOpen`).
    #[must_use]
    pub fn pages_open(count: usize) -> String {
        format!(
            "{count} persistent {} still open",
            if count == 1 { "page" } else { "pages" }
        )
    }

    /// The page has nowhere to go in that direction.
    #[must_use]
    pub fn no_history(navigation: HtmlNavigation) -> String {
        format!("no page to go {} to", navigation.wire_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/me";

    /// Upstream's `HtmlOverlayTests.grantContainment`.
    #[test]
    fn a_grant_must_contain_the_file() {
        for (file, grant, valid) in [
            ("/a/b/report.html", None, true),
            ("/a/b/report.html", Some("/a/b"), true),
            ("/a/b/report.html", Some("/a"), true),
            ("/a/b/report.html", Some("/a/b/report.html"), false),
            ("/a/b/./report.html", Some("/a/b/report.html/"), false),
            ("/a/bc/report.html", Some("/a/b"), false),
            ("/x/report.html", Some("/a"), false),
            ("relative/report.html", None, false),
            ("/a/report.html", Some("relative"), false),
        ] {
            assert_eq!(grant_error(file, grant, HOME).is_none(), valid, "{file} {grant:?}");
        }
    }

    /// Upstream's `HtmlOverlayTests.grantRefusesRootAndHome`.
    #[test]
    fn a_grant_is_never_the_root_or_home() {
        for (grant, valid) in [
            ("/", false),
            ("/Users/me", false),
            ("/Users/me/", false),
            ("/Users/me/./", false),
            ("/Users/me/docs", true),
            ("/Users", true),
        ] {
            let error = grant_error("/Users/me/docs/report.html", Some(grant), HOME);
            assert_eq!(error.is_none(), valid, "{grant}");
            if !valid {
                assert_eq!(error, Some(CWD_ROOT_OR_HOME));
            }
        }
    }

    #[test]
    fn each_refusal_names_its_reason() {
        assert_eq!(grant_error("x.html", None, HOME), Some(FILE_NOT_ABSOLUTE));
        assert_eq!(grant_error("/a/x.html", Some("a"), HOME), Some(CWD_NOT_ABSOLUTE));
        assert_eq!(grant_error("/a/x.html", Some("/a/x.html"), HOME), Some(CWD_IS_FILE));
        assert_eq!(grant_error("/b/x.html", Some("/a"), HOME), Some(OUTSIDE_CWD));
        assert_eq!(
            grant_error("/a/../b/x.html", Some("/b"), HOME),
            None,
            ".. resolves lexically"
        );
        assert!(contains("/a", "/a/b/c") && !contains("/a/b", "/a/c"));
    }

    /// Upstream's `HtmlOverlayTests.webURLAcceptsOnlyAbsoluteHttpWithAHost`.
    #[test]
    fn web_urls_are_absolute_http_with_a_host() {
        for (text, valid) in [
            ("http://localhost:5173/", true),
            ("https://example.com/docs?q=1#top", true),
            ("HTTPS://example.com", true),
            ("http://127.0.0.1:8000", true),
            ("http://[::1]:8080/", true),
            ("ftp://example.com/", false),
            ("file:///tmp/r.html", false),
            ("localhost:5173", false),
            ("/tmp/r.html", false),
            ("http://", false),
            ("javascript:alert(1)", false),
            ("", false),
            ("page.html", false),
            ("http:example.com", false),
            ("http:/example.com", false),
            (" http://example.com", false),
        ] {
            assert_eq!(web_url(text).is_some(), valid, "{text}");
        }
    }

    #[test]
    fn navigations_round_trip() {
        for navigation in [
            HtmlNavigation::Back,
            HtmlNavigation::Forward,
            HtmlNavigation::Browser,
            HtmlNavigation::Finder,
        ] {
            assert_eq!(HtmlNavigation::from_wire(navigation.wire_name()), Some(navigation));
        }
        assert_eq!(HtmlNavigation::from_wire("sideways"), None);
    }
}
