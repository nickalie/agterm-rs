//! Where a page may go (upstream's `HtmlOrigin`, `HtmlNavigationAction`, `HtmlNavigationPolicy` in
//! `HtmlOverlay.swift`): a file page stays inside its grant, a URL page's main frame on its origin, a browsing page's
//! main frame on any HTTP(S) origin; anything else a user clicked is offered to the web browser, the rest is cancelled.

use url::Url;

use super::{HtmlOverlay, HtmlSource, contains};

/// A web page's origin: lowercase scheme and host plus the effective port, so an omitted port equals the scheme's
/// default (upstream's `HtmlOrigin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlOrigin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl HtmlOrigin {
    /// The origin of an http or https URL with a host; `None` for anything else.
    #[must_use]
    pub fn new(url: &Url) -> Option<Self> {
        let scheme = url.scheme().to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let host = match url.host()? {
            url::Host::Domain(name) => name.to_ascii_lowercase(),
            url::Host::Ipv4(address) => address.to_string(),
            url::Host::Ipv6(address) => address.to_string(),
        };
        if host.is_empty() {
            return None;
        }
        let port = url.port().unwrap_or(default_port(&scheme));
        Some(Self { scheme, host, port })
    }

    /// The origin as a user compares it: the port only when it is not the scheme's default, an IPv6 host bracketed.
    #[must_use]
    pub fn display(&self) -> String {
        let name = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == default_port(&self.scheme) {
            format!("{}://{name}", self.scheme)
        } else {
            format!("{}://{name}:{}", self.scheme, self.port)
        }
    }
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

/// Where a navigation would land; a navigation without a target frame opens a new window (upstream's
/// `HtmlNavigationTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlNavigationTarget {
    MainFrame,
    Subframe,
    NewWindow,
}

/// A frame navigation the web view asks about; subresource loads never reach it (upstream's `HtmlNavigationAction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlNavigationAction {
    pub url: Url,
    pub target: HtmlNavigationTarget,
    /// The user clicked a link (`WebKitNavigationType` link-clicked).
    pub user_activated: bool,
}

/// What the policy decides (upstream's `HtmlNavigationDecision`). `OpenExternal` needs the user's confirmation,
/// because the web view reports a scripted click as a link activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlNavigationDecision {
    Allow,
    OpenExternal,
    Cancel,
}

/// Decides `action` for `overlay` (upstream's `HtmlNavigationPolicy.decide`): file navigation bounded by its grant,
/// URL main-frame navigation by its origin unless the page browses.
#[must_use]
pub fn decide(action: &HtmlNavigationAction, overlay: &HtmlOverlay) -> HtmlNavigationDecision {
    use HtmlNavigationDecision::{Allow, Cancel, OpenExternal};
    let scheme = action.url.scheme().to_ascii_lowercase();
    let web = scheme == "http" || scheme == "https";
    let offer = if action.user_activated { OpenExternal } else { Cancel };
    if action.target == HtmlNavigationTarget::NewWindow {
        return if web { offer } else { Cancel };
    }
    if scheme == "about" {
        return Allow;
    }
    match &overlay.source {
        HtmlSource::File { grant_root, .. } => {
            if web {
                return offer;
            }
            let Some(grant_root) = grant_root.as_deref().filter(|_| scheme == "file") else {
                return Cancel;
            };
            if contains(grant_root, &file_path(&action.url)) {
                Allow
            } else {
                Cancel
            }
        }
        HtmlSource::Url(original) => {
            if !web {
                return Cancel;
            }
            let same_origin = HtmlOrigin::new(&action.url) == HtmlOrigin::new(original);
            if overlay.flags.browse || action.target == HtmlNavigationTarget::Subframe || same_origin {
                Allow
            } else {
                offer
            }
        }
    }
}

/// A file URL's path, percent-decoded as Foundation's `URL.path` gives it: the OS path it names, else (a URL
/// path no local path spells, such as a drive-less one on Windows) its percent-decoded path, and the raw path when
/// that does not decode.
#[must_use]
pub fn file_path(url: &Url) -> String {
    url.to_file_path()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .or_else(|| crate::link_policy::percent_decode(url.path()))
        .unwrap_or_else(|| url.path().to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::HtmlPageFlags;
    use super::*;
    use HtmlNavigationDecision::{Allow, Cancel, OpenExternal};
    use HtmlNavigationTarget::{MainFrame, NewWindow, Subframe};

    fn action(url: &str, target: HtmlNavigationTarget, user_activated: bool) -> HtmlNavigationAction {
        HtmlNavigationAction {
            url: Url::parse(url).unwrap(),
            target,
            user_activated,
        }
    }

    fn file_page(grant_root: Option<&str>) -> HtmlOverlay {
        HtmlOverlay::new(
            HtmlSource::File {
                path: "/tmp/a/report.html".to_owned(),
                grant_root: grant_root.map(str::to_owned),
            },
            HtmlPageFlags::default(),
        )
    }

    fn url_page(browse: bool) -> HtmlOverlay {
        HtmlOverlay::new(
            HtmlSource::Url(Url::parse("http://localhost:5173/").unwrap()),
            HtmlPageFlags {
                browse,
                ..HtmlPageFlags::default()
            },
        )
    }

    fn origin(url: &str) -> HtmlOrigin {
        HtmlOrigin::new(&Url::parse(url).unwrap()).unwrap()
    }

    /// Upstream's `HtmlOverlayTests.navigationPolicy`: a file page with and without a grant.
    #[test]
    fn a_file_page_stays_inside_its_grant() {
        for (url, target, user, grant, decision) in [
            ("file:///tmp/a/report.html", MainFrame, false, None, Cancel),
            ("file:///tmp/a/report.html", MainFrame, false, Some("/tmp/a"), Allow),
            (
                "file:///tmp/a/report.html#section",
                MainFrame,
                true,
                Some("/tmp/a"),
                Allow,
            ),
            ("file:///tmp/a/other.html", MainFrame, true, None, Cancel),
            ("about:blank", MainFrame, false, None, Allow),
            ("about:blank#section", MainFrame, true, None, Allow),
            ("file:///tmp/a/other.html", MainFrame, true, Some("/tmp/a"), Allow),
            ("file:///tmp/a/frame.html", Subframe, false, Some("/tmp/a"), Allow),
            ("file:///etc/passwd", MainFrame, true, Some("/tmp/a"), Cancel),
            ("file:///tmp/ab/x.html", Subframe, false, Some("/tmp/a"), Cancel),
            ("https://example.com/", MainFrame, true, None, OpenExternal),
            ("http://example.com/", MainFrame, true, None, OpenExternal),
            ("https://example.com/", MainFrame, false, None, Cancel),
            ("https://example.com/", Subframe, false, None, Cancel),
            ("https://example.com/", Subframe, true, None, OpenExternal),
            ("https://example.com/", NewWindow, true, None, OpenExternal),
            ("https://example.com/", NewWindow, false, None, Cancel),
            ("file:///tmp/a/report.html", NewWindow, true, None, Cancel),
            ("about:blank", Subframe, false, None, Allow),
            ("mailto:a@example.com", MainFrame, true, None, Cancel),
            ("x-custom://open", MainFrame, true, None, Cancel),
            ("data:text/html,hi", MainFrame, false, None, Cancel),
        ] {
            let decided = decide(&action(url, target, user), &file_page(grant));
            assert_eq!(decided, decision, "{url} {target:?} {user} {grant:?}");
        }
    }

    #[test]
    fn a_file_url_is_compared_by_its_decoded_lexical_path() {
        for (url, decision) in [
            ("file:///tmp/a/b/../c.html", Allow),
            ("file:///tmp/a/../b/x.html", Cancel),
            ("file:///tmp/a/my%20page.html", Allow),
        ] {
            let decided = decide(&action(url, MainFrame, false), &file_page(Some("/tmp/a")));
            assert_eq!(decided, decision, "{url}");
        }
        let spaced = Url::parse("file:///tmp/a/my%20page.html").unwrap();
        assert_eq!(file_path(&spaced), "/tmp/a/my page.html");
        // a path that decodes to no UTF-8 stays as written
        let raw = Url::parse("file:///tmp/a/%FF.html").unwrap();
        assert_eq!(file_path(&raw), "/tmp/a/%FF.html");
    }

    /// Upstream's `HtmlOverlayTests.urlPagePolicy`: a URL page's main frame is pinned to its origin.
    #[test]
    fn a_url_page_is_pinned_to_its_origin() {
        for (url, target, user, decision) in [
            ("http://localhost:5173/about", MainFrame, false, Allow),
            ("http://LOCALHOST:5173/", MainFrame, true, Allow),
            ("http://localhost:5174/", MainFrame, false, Cancel),
            ("http://localhost:5174/", MainFrame, true, OpenExternal),
            ("https://localhost:5173/", MainFrame, false, Cancel),
            ("https://example.com/", MainFrame, true, OpenExternal),
            ("https://example.com/", MainFrame, false, Cancel),
            ("https://example.com/embed", Subframe, false, Allow),
            ("http://localhost:5173/", NewWindow, true, OpenExternal),
            ("http://localhost:5173/", NewWindow, false, Cancel),
            ("about:blank", MainFrame, false, Allow),
            ("about:srcdoc", Subframe, false, Allow),
            ("file:///tmp/a/report.html", MainFrame, true, Cancel),
            ("file:///tmp/a/report.html", Subframe, false, Cancel),
            ("mailto:a@example.com", MainFrame, true, Cancel),
        ] {
            let decided = decide(&action(url, target, user), &url_page(false));
            assert_eq!(decided, decision, "{url} {target:?} {user}");
        }
    }

    /// Upstream's `HtmlOverlayTests.browsingPagePolicy`: a browsing page's main frame goes to any HTTP(S) origin.
    #[test]
    fn a_browsing_page_goes_to_any_web_origin() {
        for (url, target, user, decision) in [
            ("http://localhost:5173/about", MainFrame, false, Allow),
            ("https://example.com/", MainFrame, true, Allow),
            ("https://example.com/", MainFrame, false, Allow),
            ("http://localhost:5174/", MainFrame, false, Allow),
            ("https://example.com/embed", Subframe, false, Allow),
            ("https://example.com/", NewWindow, true, OpenExternal),
            ("https://example.com/", NewWindow, false, Cancel),
            ("about:blank", MainFrame, false, Allow),
            ("file:///tmp/a/report.html", MainFrame, true, Cancel),
            ("file:///tmp/a/report.html", Subframe, false, Cancel),
            ("mailto:a@example.com", MainFrame, true, Cancel),
            ("x-custom://open", MainFrame, true, Cancel),
            ("data:text/html,hi", MainFrame, false, Cancel),
        ] {
            let decided = decide(&action(url, target, user), &url_page(true));
            assert_eq!(decided, decision, "{url} {target:?} {user}");
        }
    }

    /// Upstream's `HtmlOverlayTests.originEquality`.
    #[test]
    fn an_origin_is_scheme_host_and_effective_port() {
        for (left, right, same) in [
            ("http://example.com/", "http://EXAMPLE.com:80/a", true),
            ("https://example.com/", "https://example.com:443/b", true),
            ("HTTPS://example.com/", "https://example.com/", true),
            ("http://example.com/", "https://example.com/", false),
            ("http://example.com/", "http://example.com:8080/", false),
            ("http://example.com/", "http://www.example.com/", false),
            ("http://[::1]:8080/", "http://[::1]:8080/x", true),
        ] {
            assert_eq!(origin(left) == origin(right), same, "{left} {right}");
        }
    }

    #[test]
    fn only_a_web_url_with_a_host_has_an_origin() {
        for url in [
            "file:///tmp/a.html",
            "about:blank",
            "ftp://example.com/",
            "data:text/html,hi",
        ] {
            assert_eq!(HtmlOrigin::new(&Url::parse(url).unwrap()), None, "{url}");
        }
    }

    /// Upstream's `HtmlOverlayTests.originNamesTheRealHost`, and the display's default ports and IPv6 brackets.
    #[test]
    fn an_origin_displays_the_real_host_and_only_a_non_default_port() {
        for (address, display) in [
            (
                "https://trusted.example@attacker.example/path",
                "https://attacker.example",
            ),
            ("http://127.0.0.1:5173/", "http://127.0.0.1:5173"),
            ("http://example.com:80/", "http://example.com"),
            ("https://Example.com:443/docs", "https://example.com"),
            ("http://example.com:443/", "http://example.com:443"),
            ("https://example.com:80/", "https://example.com:80"),
            ("http://[::1]:8080/", "http://[::1]:8080"),
            ("https://[::1]/", "https://[::1]"),
        ] {
            let url = Url::parse(address).unwrap();
            assert_eq!(HtmlSource::origin_of(&url).as_deref(), Some(display), "{address}");
        }
    }
}
