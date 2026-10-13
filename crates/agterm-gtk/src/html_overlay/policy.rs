//! The page's `WebKitGTK` specifics as pure functions: how a `WebKitGTK` policy decision maps onto the shared navigation
//! policy, how a load's events settle into the state the model reads (upstream's `loadPending`/`committed` in
//! `HtmlOverlayPage`), what the page reports as shown (upstream's `pageURL`, `browserURL`, `urlChanged`), the grant's
//! own URI scheme, the strip's buttons (upstream's `HtmlOverlayView.strip`), and how a page follows a changed theme
//! (upstream's `applyTheme`).
//!
//! `WebKitGTK` differs from `WKWebView` in two ways that shape this (the Linux fork met both): a navigation action names
//! no target frame, so the main frame's origin is checked again on its response, which does say whether it is the
//! main frame's; and it has no `loadFileURL(_:allowingReadAccessTo:)`, so a granted file page loads through
//! [`GRANT_SCHEME`], whose handler serves only files inside the grant.

use std::path::{Path, PathBuf};

use agterm_core::html::navigation::file_path;
use agterm_core::html::{
    HtmlNavigationAction, HtmlNavigationTarget, HtmlOverlay, HtmlOverlayTheme, HtmlPageInfo, HtmlSource,
};
use url::Url;

/// The scheme a granted file page loads through: every read passes the handler, which answers only inside the grant,
/// while `file:` stays a local scheme `WebKit` refuses to a non-local origin.
pub const GRANT_SCHEME: &str = "agterm-file";

/// What `WebKitGTK` tells about a decision: a frame navigation (no target frame named), a new window, or the response
/// of the main frame or a subframe (the fork's `LinuxHtmlTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionTarget {
    Frame,
    NewWindow,
    MainResponse,
    SubframeResponse,
}

/// The shared policy's view of a `WebKitGTK` decision: a clicked link is judged as the main frame's, where a hand-off
/// belongs; any other navigation as a subframe's, which admits what an iframe may load, the main frame being pinned
/// when its response arrives. Only a navigation or a new window counts as clicked.
#[must_use]
pub fn navigation_action(url: Url, target: DecisionTarget, link_clicked: bool) -> HtmlNavigationAction {
    let frame = match target {
        DecisionTarget::NewWindow => HtmlNavigationTarget::NewWindow,
        DecisionTarget::MainResponse => HtmlNavigationTarget::MainFrame,
        DecisionTarget::SubframeResponse => HtmlNavigationTarget::Subframe,
        DecisionTarget::Frame => {
            if link_clicked {
                HtmlNavigationTarget::MainFrame
            } else {
                HtmlNavigationTarget::Subframe
            }
        }
    };
    let user_activated = link_clicked && matches!(target, DecisionTarget::Frame | DecisionTarget::NewWindow);
    HtmlNavigationAction {
        url,
        target: frame,
        user_activated,
    }
}

/// Whether a navigation or new-window request goes ahead in the page's own view. A real `file:` address would bypass
/// the grant and is never asked about; any other request asks the policy (`allows`, which raises a hand-off prompt the
/// policy offers), a new window's too, since that prompt is all a new window can lead to: the window itself never
/// opens (upstream's `decidePolicyFor` with a nil target frame).
#[must_use]
pub fn navigation_proceeds(uri: &str, new_window: bool, allows: impl FnOnce() -> bool) -> bool {
    let allowed = !uri.starts_with("file:") && allows();
    allowed && !new_window
}

/// A web view address as the app spells it: a granted file as the `file:` URI it stands for.
#[must_use]
pub fn app_uri(uri: &str) -> String {
    match uri.strip_prefix(GRANT_SCHEME).and_then(|rest| rest.strip_prefix(':')) {
        Some(rest) => format!("file:{rest}"),
        None => uri.to_owned(),
    }
}

/// The address a granted file page loads `path` from; `None` for a path that is not absolute.
#[must_use]
pub fn granted_uri(path: &str) -> Option<String> {
    let file = Url::from_file_path(path).ok()?;
    let rest = file.as_str().strip_prefix("file:")?;
    Some(format!("{GRANT_SCHEME}:{rest}"))
}

/// Why the grant's handler refuses a read.
pub const OUTSIDE_GRANT: &str = "outside the page's granted directory";
pub const NOT_A_FILE: &str = "not a file";

/// The file a grant's read of `requested` serves: its real path when that sits strictly inside `root` (itself a real
/// path) and is a regular file.
///
/// # Errors
///
/// The refusal's text: no grant, a path outside it, or not a regular file.
pub fn granted_file(root: Option<&Path>, requested: Option<&Path>) -> Result<PathBuf, &'static str> {
    let (Some(root), Some(requested)) = (root, requested) else {
        return Err(OUTSIDE_GRANT);
    };
    let resolved = requested.canonicalize().map_err(|_| OUTSIDE_GRANT)?;
    if resolved == root || !resolved.starts_with(root) {
        return Err(OUTSIDE_GRANT);
    }
    if !resolved.is_file() {
        return Err(NOT_A_FILE);
    }
    Ok(resolved)
}

/// One thing a page's main-frame load reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadEvent {
    Started,
    Committed,
    Finished,
    /// A load superseded by the next one; that one has its own outcome.
    Cancelled,
    Terminated,
    /// `WebKit`'s policy-change interruption: a policy cancel, a redirect handed off, a response it cannot show.
    Interrupted(String),
    Failed(String),
}

/// What the model is told after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    None,
    Loading,
    Loaded,
    Failed(String),
}

/// Keeps a page's main-frame load ending loaded or failed (upstream's `loadPending` and `committed`): `pending` is a
/// load in flight, explicit or started by the page, `committed` a document the web process still shows, which an
/// interrupted load leaves in place, and `explicit` that the app started the pending load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadTracker {
    pending: bool,
    committed: bool,
    explicit: bool,
}

impl LoadTracker {
    /// The app starts a load.
    pub fn begin(&mut self) {
        self.pending = true;
        self.explicit = true;
    }

    /// Whether the web process still shows a document (upstream's `committed`).
    #[must_use]
    pub fn committed(self) -> bool {
        self.committed
    }

    /// Settles `event`.
    pub fn handle(&mut self, event: &LoadEvent) -> LoadOutcome {
        match event {
            LoadEvent::Started => {
                self.pending = true;
                LoadOutcome::Loading
            }
            LoadEvent::Committed => {
                self.committed = true;
                LoadOutcome::None
            }
            // `WebKitGTK` also finishes a load it reported failed: the failure stands
            LoadEvent::Finished if self.pending => self.settle(),
            LoadEvent::Finished | LoadEvent::Cancelled => LoadOutcome::None,
            LoadEvent::Interrupted(_) if !self.pending => LoadOutcome::None,
            LoadEvent::Interrupted(message) if !self.committed => self.fail(message.clone()),
            LoadEvent::Interrupted(_) => self.settle(),
            LoadEvent::Failed(message) => self.fail(message.clone()),
            LoadEvent::Terminated => {
                self.committed = false;
                self.fail("web content process terminated".to_owned())
            }
        }
    }

    /// The policy refused the main frame's response for `url`. `WebKitGTK` asks once the load started, where `WebKit` on
    /// macOS refuses a page's own navigation before it starts and the shown document stays, so a load the app started,
    /// or one with nothing shown, fails here; a page's own load over a shown document settles.
    pub fn blocked(&mut self, url: &str) -> LoadOutcome {
        if !self.pending {
            return LoadOutcome::None;
        }
        if !self.explicit && self.committed {
            return self.settle();
        }
        self.fail(format!("navigation blocked: {url}"))
    }

    /// A failure outside `WebKit`'s events (a store or a file that cannot be read).
    pub fn fail(&mut self, message: String) -> LoadOutcome {
        self.pending = false;
        self.explicit = false;
        LoadOutcome::Failed(message)
    }

    fn settle(&mut self) -> LoadOutcome {
        self.pending = false;
        self.explicit = false;
        LoadOutcome::Loaded
    }
}

/// A file path's URL; `None` for a path that is not absolute.
fn file_url(path: &str) -> Option<Url> {
    Url::from_file_path(path).ok()
}

/// The source's own address: a file page's file, a URL page's URL.
fn source_url(source: &HtmlSource) -> Option<Url> {
    match source {
        HtmlSource::File { path, .. } => file_url(path),
        HtmlSource::Url(url) => Some(url.clone()),
    }
}

/// What the page shows (upstream's `pageURL`): a browsing page the address taken at commit (its source before one), a
/// text-loaded file page its file, else the web view's address unless it is an `about:` one, else the source.
/// `web` is the web view's address as the app spells it.
#[must_use]
pub fn page_url(overlay: &HtmlOverlay, shown: Option<&Url>, web: Option<&Url>) -> Option<Url> {
    if overlay.flags.browse
        && let HtmlSource::Url(source) = &overlay.source
    {
        return Some(shown.unwrap_or(source).clone());
    }
    if overlay.text_loaded()
        && let HtmlSource::File { path, .. } = &overlay.source
    {
        return file_url(path);
    }
    if let Some(web) = web.filter(|web| web.scheme() != "about") {
        return Some(web.clone());
    }
    source_url(&overlay.source)
}

/// The page report (upstream's `reportPage`): a file address as its path, a title only when not empty.
#[must_use]
pub fn page_info(url: &Url, title: Option<&str>, can_go_back: bool, can_go_forward: bool) -> HtmlPageInfo {
    HtmlPageInfo {
        page: if url.scheme() == "file" {
            file_path(url)
        } else {
            url.as_str().to_owned()
        },
        title: title.filter(|title| !title.is_empty()).map(str::to_owned),
        can_go_back,
        can_go_forward,
    }
}

/// What a hand-off to the web browser opens (upstream's `browserURL`): a file page its own file, never one it navigated
/// to; a URL page what it shows (a browsing page the address taken at commit) while that is HTTP(S), else its source.
#[must_use]
pub fn browser_url(overlay: &HtmlOverlay, shown: Option<&Url>, web: Option<&Url>) -> Option<Url> {
    match &overlay.source {
        HtmlSource::File { path, .. } => file_url(path),
        HtmlSource::Url(original) => {
            let current = if overlay.flags.browse { shown } else { web };
            Some(
                current
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                    .unwrap_or(original)
                    .clone(),
            )
        }
    }
}

/// The address a browsing page shows after the web view's address became `web` (upstream's `urlChanged`): a pending
/// load names its address before its document replaces the shown one, so a change of origin waits for the commit,
/// while one on the shown origin (a fragment, `pushState`) is taken at once. Two addresses without a web origin
/// compare equal, as upstream's optional origins do.
#[must_use]
pub fn shown_after_change(shown: Option<&Url>, web: Option<&Url>) -> Option<Url> {
    match (shown, web) {
        (Some(shown), Some(web)) if HtmlSource::origin_of(web) == HtmlSource::origin_of(shown) => Some(web.clone()),
        (shown, _) => shown.cloned(),
    }
}

/// One item of the identity strip, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripItem {
    Back,
    Forward,
    Reload,
    /// The identity label and the page's title.
    Identity,
    Browser,
    /// Show in the file manager: a file page's.
    Files,
    /// Copy the address: a URL page's.
    CopyLink,
    Close,
}

/// The strip of `overlay` (upstream's `HtmlOverlayView.strip`): `None` for a chromeless page; navigation adds the
/// history and reload buttons before the identity and the hand-offs after it; the close button always ends it.
#[must_use]
pub fn strip_items(overlay: &HtmlOverlay) -> Option<Vec<StripItem>> {
    if overlay.flags.chromeless {
        return None;
    }
    let mut items = Vec::new();
    if overlay.flags.navigation {
        items.extend([StripItem::Back, StripItem::Forward, StripItem::Reload]);
    }
    items.push(StripItem::Identity);
    if overlay.flags.navigation {
        items.push(StripItem::Browser);
        items.push(match overlay.source {
            HtmlSource::File { .. } => StripItem::Files,
            HtmlSource::Url(_) => StripItem::CopyLink,
        });
    }
    items.push(StripItem::Close);
    Some(items)
}

/// The strip's tooltip on the identity (upstream's `.help(overlay.current?.page ?? sourceText)`).
#[must_use]
pub fn identity_tooltip(overlay: &HtmlOverlay) -> String {
    overlay
        .current
        .as_ref()
        .map_or_else(|| overlay.source.text(), |current| current.page.clone())
}

/// What a page does when the terminal theme may have changed (upstream's `applyTheme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChange {
    /// The theme is the one the page already has.
    Unchanged,
    /// The page's scripts are installed again with the new theme, which its next load wears: a URL page.
    Reinstall,
    /// The scripts are installed again and the page reloads what it shows to wear the theme now: a themed (file) page.
    ReinstallAndReload,
}

/// How a page showing `current` follows `next`; `themed` is a file page. Nothing is evaluated in the live document:
/// script the app runs there carries a user gesture the page's own code could borrow.
#[must_use]
pub fn theme_change(current: &HtmlOverlayTheme, next: &HtmlOverlayTheme, themed: bool) -> ThemeChange {
    if current == next {
        ThemeChange::Unchanged
    } else if themed {
        ThemeChange::ReinstallAndReload
    } else {
        ThemeChange::Reinstall
    }
}

#[cfg(test)]
mod tests {
    use agterm_core::html::{HtmlNavigationDecision, HtmlPageFlags, decide};

    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn file_page(grant: Option<&str>, flags: HtmlPageFlags) -> HtmlOverlay {
        HtmlOverlay::new(
            HtmlSource::File {
                path: "/site/index.html".to_owned(),
                grant_root: grant.map(str::to_owned),
            },
            flags,
        )
    }

    fn url_page(flags: HtmlPageFlags) -> HtmlOverlay {
        HtmlOverlay::new(HtmlSource::Url(url("https://example.com/start")), flags)
    }

    #[test]
    fn a_new_window_asks_the_policy_for_its_prompt_and_never_proceeds() {
        let count = std::cell::Cell::new(0);
        let asked = &count;
        let allows = |answer: bool| {
            move || {
                asked.set(asked.get() + 1);
                answer
            }
        };
        assert!(!navigation_proceeds("https://a.test/", true, allows(true)));
        assert!(!navigation_proceeds("https://a.test/", true, allows(false)));
        assert_eq!(
            asked.get(),
            2,
            "a new window's link is asked about, so its hand-off prompt shows"
        );
        assert!(navigation_proceeds("https://a.test/", false, allows(true)));
        assert!(!navigation_proceeds("https://a.test/", false, allows(false)));
        assert_eq!(asked.get(), 4);
        assert!(!navigation_proceeds("file:///etc/passwd", false, allows(true)));
        assert!(!navigation_proceeds("file:///etc/passwd", true, allows(true)));
        assert_eq!(asked.get(), 4, "a real file: address is never asked about");
    }

    #[test]
    fn a_clicked_navigation_is_the_main_frames_and_anything_else_a_subframes() {
        let clicked = navigation_action(url("https://a.test/"), DecisionTarget::Frame, true);
        assert_eq!(
            (clicked.target, clicked.user_activated),
            (HtmlNavigationTarget::MainFrame, true)
        );
        let scripted = navigation_action(url("https://a.test/"), DecisionTarget::Frame, false);
        assert_eq!(
            (scripted.target, scripted.user_activated),
            (HtmlNavigationTarget::Subframe, false)
        );
        let window = navigation_action(url("https://a.test/"), DecisionTarget::NewWindow, true);
        assert_eq!(
            (window.target, window.user_activated),
            (HtmlNavigationTarget::NewWindow, true)
        );
        let response = navigation_action(url("https://a.test/"), DecisionTarget::MainResponse, true);
        assert_eq!(
            (response.target, response.user_activated),
            (HtmlNavigationTarget::MainFrame, false)
        );
        let sub = navigation_action(url("https://a.test/"), DecisionTarget::SubframeResponse, false);
        assert_eq!(sub.target, HtmlNavigationTarget::Subframe);
    }

    #[test]
    fn a_url_page_is_pinned_at_its_main_response_and_offers_clicked_links_away() {
        let page = url_page(HtmlPageFlags::default());
        let decide_at = |text: &str, target, clicked| decide(&navigation_action(url(text), target, clicked), &page);
        assert_eq!(
            decide_at("https://other.test/", DecisionTarget::Frame, false),
            HtmlNavigationDecision::Allow,
            "a redirect is judged at its response"
        );
        assert_eq!(
            decide_at("https://other.test/", DecisionTarget::MainResponse, false),
            HtmlNavigationDecision::Cancel
        );
        assert_eq!(
            decide_at("https://example.com/next", DecisionTarget::MainResponse, false),
            HtmlNavigationDecision::Allow
        );
        assert_eq!(
            decide_at("https://other.test/", DecisionTarget::Frame, true),
            HtmlNavigationDecision::OpenExternal
        );
        assert_eq!(
            decide_at("https://other.test/", DecisionTarget::SubframeResponse, false),
            HtmlNavigationDecision::Allow
        );
    }

    #[test]
    fn a_granted_file_reads_back_as_its_file_uri() {
        let granted = granted_uri("/site/a b.html").unwrap();
        assert_eq!(granted, "agterm-file:///site/a%20b.html");
        assert_eq!(app_uri(&granted), "file:///site/a%20b.html");
        assert_eq!(app_uri("https://example.com/"), "https://example.com/");
        assert_eq!(app_uri("agterm-filex:///x"), "agterm-filex:///x");
        assert_eq!(granted_uri("relative.html"), None);
    }

    #[test]
    fn the_grant_serves_only_regular_files_strictly_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/page.html"), "x").unwrap();
        assert_eq!(
            granted_file(Some(&root), Some(&root.join("sub/../sub/page.html"))),
            Ok(root.join("sub/page.html"))
        );
        assert_eq!(granted_file(Some(&root), Some(&root.join("sub"))), Err(NOT_A_FILE));
        assert_eq!(granted_file(Some(&root), Some(&root)), Err(OUTSIDE_GRANT));
        assert_eq!(
            granted_file(Some(&root), Some(&root.join("../x.html"))),
            Err(OUTSIDE_GRANT)
        );
        assert_eq!(
            granted_file(Some(&root), Some(&root.join("missing.html"))),
            Err(OUTSIDE_GRANT)
        );
        assert_eq!(
            granted_file(None, Some(&root.join("sub/page.html"))),
            Err(OUTSIDE_GRANT)
        );
    }

    #[test]
    fn a_load_ends_loaded_or_failed() {
        let mut load = LoadTracker::default();
        load.begin();
        assert_eq!(load.handle(&LoadEvent::Started), LoadOutcome::Loading);
        assert_eq!(load.handle(&LoadEvent::Committed), LoadOutcome::None);
        assert_eq!(load.handle(&LoadEvent::Finished), LoadOutcome::Loaded);
        assert_eq!(load.handle(&LoadEvent::Finished), LoadOutcome::None, "nothing pending");

        let mut failed = LoadTracker::default();
        failed.begin();
        assert_eq!(
            failed.handle(&LoadEvent::Failed("no host".to_owned())),
            LoadOutcome::Failed("no host".to_owned())
        );
        assert_eq!(
            failed.handle(&LoadEvent::Finished),
            LoadOutcome::None,
            "the failure stands"
        );
        assert_eq!(failed.handle(&LoadEvent::Cancelled), LoadOutcome::None);
    }

    #[test]
    fn an_interruption_fails_only_a_load_with_nothing_shown() {
        let interrupted = LoadEvent::Interrupted("Frame load was interrupted".to_owned());
        let mut fresh = LoadTracker::default();
        fresh.begin();
        assert_eq!(
            fresh.handle(&interrupted),
            LoadOutcome::Failed("Frame load was interrupted".to_owned())
        );
        let mut shown = LoadTracker::default();
        shown.handle(&LoadEvent::Committed);
        shown.handle(&LoadEvent::Started);
        assert_eq!(
            shown.handle(&interrupted),
            LoadOutcome::Loaded,
            "the shown document stays"
        );
        assert_eq!(shown.handle(&interrupted), LoadOutcome::None, "nothing pending");
    }

    #[test]
    fn a_blocked_response_fails_an_app_load_or_one_with_nothing_shown() {
        let mut explicit = LoadTracker::default();
        explicit.handle(&LoadEvent::Committed);
        explicit.begin();
        assert_eq!(
            explicit.blocked("https://other.test/"),
            LoadOutcome::Failed("navigation blocked: https://other.test/".to_owned())
        );
        let mut own = LoadTracker::default();
        own.handle(&LoadEvent::Committed);
        own.handle(&LoadEvent::Started);
        assert_eq!(own.blocked("https://other.test/"), LoadOutcome::Loaded);
        assert_eq!(LoadTracker::default().blocked("https://x.test/"), LoadOutcome::None);
        let mut gone = LoadTracker::default();
        gone.handle(&LoadEvent::Committed);
        assert_eq!(
            gone.handle(&LoadEvent::Terminated),
            LoadOutcome::Failed("web content process terminated".to_owned())
        );
        assert!(!gone.committed());
    }

    #[test]
    fn the_page_shown_follows_upstreams_page_url() {
        let browse = url_page(HtmlPageFlags {
            browse: true,
            ..HtmlPageFlags::default()
        });
        let shown = url("https://moved.test/x");
        assert_eq!(
            page_url(&browse, None, Some(&shown)),
            Some(url("https://example.com/start"))
        );
        assert_eq!(page_url(&browse, Some(&shown), None), Some(shown.clone()));
        let text = file_page(None, HtmlPageFlags::default());
        assert_eq!(
            page_url(&text, None, Some(&url("about:blank"))),
            Some(url("file:///site/index.html"))
        );
        let granted = file_page(Some("/site"), HtmlPageFlags::default());
        let other = url("file:///site/other.html");
        assert_eq!(page_url(&granted, None, Some(&other)), Some(other.clone()));
        assert_eq!(
            page_url(&granted, None, Some(&url("about:blank"))),
            Some(url("file:///site/index.html"))
        );
        let info = page_info(&url("file:///site/a%20b.html"), Some(""), true, false);
        assert_eq!(
            info,
            HtmlPageInfo {
                page: "/site/a b.html".to_owned(),
                title: None,
                can_go_back: true,
                can_go_forward: false,
            }
        );
        assert_eq!(
            page_info(&url("https://example.com/"), Some("Home"), false, false).title,
            Some("Home".to_owned())
        );
    }

    #[test]
    fn the_browser_gets_a_file_pages_own_file_and_a_url_pages_web_address() {
        let granted = file_page(Some("/site"), HtmlPageFlags::default());
        assert_eq!(
            browser_url(&granted, None, Some(&url("file:///site/other.html"))),
            Some(url("file:///site/index.html"))
        );
        let pinned = url_page(HtmlPageFlags::default());
        let next = url("https://example.com/next");
        assert_eq!(browser_url(&pinned, None, Some(&next)), Some(next.clone()));
        assert_eq!(
            browser_url(&pinned, None, Some(&url("about:blank"))),
            Some(url("https://example.com/start"))
        );
        let browse = url_page(HtmlPageFlags {
            browse: true,
            ..HtmlPageFlags::default()
        });
        assert_eq!(
            browser_url(&browse, None, Some(&next)),
            Some(url("https://example.com/start")),
            "a browsing page takes its address at commit"
        );
    }

    #[test]
    fn a_shown_address_moves_on_its_origin_only() {
        let shown = url("https://a.test/one");
        let same = url("https://a.test/one#two");
        assert_eq!(shown_after_change(Some(&shown), Some(&same)), Some(same.clone()));
        assert_eq!(
            shown_after_change(Some(&shown), Some(&url("https://b.test/"))),
            Some(shown.clone())
        );
        assert_eq!(shown_after_change(None, Some(&same)), None);
        let file = url("file:///a.html");
        assert_eq!(
            shown_after_change(Some(&file), Some(&url("file:///b.html"))),
            Some(url("file:///b.html")),
            "no web origin on either side compares equal"
        );
    }

    #[test]
    fn the_strip_follows_the_flags_and_the_source() {
        use StripItem::{Back, Browser, Close, CopyLink, Files, Forward, Identity, Reload};
        assert_eq!(
            strip_items(&file_page(None, HtmlPageFlags::default())),
            Some(vec![Identity, Close])
        );
        let navigation = HtmlPageFlags {
            navigation: true,
            ..HtmlPageFlags::default()
        };
        assert_eq!(
            strip_items(&file_page(None, navigation)),
            Some(vec![Back, Forward, Reload, Identity, Browser, Files, Close])
        );
        assert_eq!(
            strip_items(&url_page(navigation)),
            Some(vec![Back, Forward, Reload, Identity, Browser, CopyLink, Close])
        );
        let chromeless = HtmlPageFlags {
            chromeless: true,
            navigation: true,
            ..HtmlPageFlags::default()
        };
        assert_eq!(strip_items(&file_page(None, chromeless)), None);
    }

    #[test]
    fn the_identity_tooltip_names_the_page_shown_else_the_source() {
        let mut page = url_page(HtmlPageFlags::default());
        assert_eq!(identity_tooltip(&page), "https://example.com/start");
        page.current = Some(page_info(&url("https://example.com/next"), None, false, false));
        assert_eq!(identity_tooltip(&page), "https://example.com/next");
    }

    #[test]
    fn a_changed_theme_reinstalls_and_reloads_only_a_file_page() {
        let palette: Vec<String> = (0..16).map(|slot| format!("#{slot:02x}0000")).collect();
        let dark = HtmlOverlayTheme::new("#101010", "#eeeeee", true, &palette);
        let light = HtmlOverlayTheme::new("#fafafa", "#111111", false, &palette);
        assert_eq!(theme_change(&dark, &dark.clone(), true), ThemeChange::Unchanged);
        assert_eq!(theme_change(&dark, &dark.clone(), false), ThemeChange::Unchanged);
        assert_eq!(theme_change(&dark, &light, true), ThemeChange::ReinstallAndReload);
        assert_eq!(
            theme_change(&dark, &light, false),
            ThemeChange::Reinstall,
            "a URL page gets the new variables at its next load only"
        );
        let recolored = HtmlOverlayTheme::new("#101010", "#eeeeee", true, &[]);
        assert_eq!(
            theme_change(&dark, &recolored, false),
            ThemeChange::Reinstall,
            "a palette change alone is a change"
        );
    }
}
