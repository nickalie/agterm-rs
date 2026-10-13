//! HTML overlays: the host-free rules `session.overlay.*` checks a page against before any web view exists
//! (upstream's `HtmlOverlay.swift`: `grantError`, `HtmlSource.webURL`, `HtmlOrigin`, `HtmlNavigation`). The pages
//! themselves, their bridge, navigation policy and outcomes join with the HTML overlay task.

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
