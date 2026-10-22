//! Tests of the page value and the zoom ladder (upstream's `HtmlOverlayTests`).

use url::Url;

use super::*;

fn file(path: &str, grant_root: Option<&str>) -> HtmlOverlay {
    HtmlOverlay::new(
        HtmlSource::File {
            path: path.to_owned(),
            grant_root: grant_root.map(str::to_owned),
        },
        HtmlPageFlags::default(),
    )
}

fn web(address: &str, browse: bool) -> HtmlOverlay {
    HtmlOverlay::new(
        HtmlSource::Url(Url::parse(address).unwrap()),
        HtmlPageFlags {
            browse,
            ..HtmlPageFlags::default()
        },
    )
}

fn shown(page: &str, title: Option<&str>, can_go_back: bool) -> HtmlPageInfo {
    HtmlPageInfo {
        page: page.to_owned(),
        title: title.map(str::to_owned),
        can_go_back,
        can_go_forward: false,
    }
}

/// Upstream's `HtmlOverlayTests.fileIdentityIsTheFileShownNeverTheTitle`.
#[test]
fn file_identity_is_the_file_shown_never_the_title() {
    let mut overlay = file("/a/docs/index.html", Some("/a/docs"));
    assert_eq!(overlay.identity(), "index.html");
    overlay.current = Some(shown("/a/docs/b.html", Some("ngterm: enter your password"), true));
    assert_eq!(overlay.identity(), "b.html");
}

/// Upstream's `HtmlOverlayTests.urlIdentityIsTheOrigin`.
#[test]
fn url_identity_is_the_origin() {
    for (address, identity) in [
        ("http://localhost:5173/app?x=1", "http://localhost:5173"),
        ("https://Example.com:443/docs", "https://example.com"),
        ("http://example.com:8080/", "http://example.com:8080"),
        ("http://[::1]:8080/", "http://[::1]:8080"),
    ] {
        let mut overlay = web(address, false);
        overlay.current = Some(shown(address, Some("ngterm: enter your password"), false));
        assert_eq!(overlay.identity(), identity, "{address}");
    }
}

/// Upstream's `HtmlOverlayTests.browsingIdentityIsTheOriginOfTheShownDocument`.
#[test]
fn browsing_identity_is_the_origin_of_the_shown_document() {
    for (page, identity) in [
        ("https://login.example/sso?next=1", "https://login.example"),
        ("http://localhost:5173/other", "http://localhost:5173"),
        ("about:blank", "about:blank"),
    ] {
        let mut overlay = web("http://localhost:5173/app", true);
        assert_eq!(overlay.identity(), "http://localhost:5173");
        overlay.current = Some(shown(page, Some("https://bank.example"), true));
        assert_eq!(overlay.identity(), identity, "{page}");
    }
}

/// Upstream's `HtmlOverlayTests.anOrdinaryUrlPageKeepsNamingItsSourceOrigin`.
#[test]
fn an_ordinary_url_page_keeps_naming_its_source_origin() {
    let mut overlay = web("http://localhost:5173/app", false);
    overlay.current = Some(shown("https://login.example/sso", None, true));
    assert_eq!(overlay.identity(), "http://localhost:5173");
}

#[test]
fn a_text_loaded_page_is_a_file_page_without_a_grant() {
    assert!(file("/a/r.html", None).text_loaded());
    assert!(!file("/a/r.html", Some("/a")).text_loaded());
    assert!(!web("http://localhost:5173/", false).text_loaded());
}

#[test]
fn only_a_file_page_is_themed() {
    assert!(file("/a/r.html", None).themed());
    assert!(file("/a/r.html", Some("/a")).themed());
    assert!(!web("http://localhost:5173/", false).themed());
    assert!(!web("http://localhost:5173/", true).themed());
}

/// Upstream's `HtmlOverlayTests.pageJavaScriptIsOffUnlessAsked`, and a new page's starting state.
#[test]
fn a_new_page_starts_loading_with_every_flag_off() {
    let overlay = file("/a/r.html", None);
    assert_eq!(overlay.flags, HtmlPageFlags::default());
    assert!(!overlay.flags.javascript && !overlay.flags.persistent);
    assert_eq!(overlay.load_state, HtmlLoadState::Loading);
    assert_eq!((overlay.current.clone(), overlay.load_error.clone()), (None, None));
    assert_eq!(overlay.reload_revision, 0);
    assert_eq!(overlay.reload_target, HtmlReloadTarget::Original);
    assert_ne!(overlay.id, file("/a/r.html", None).id);
}

/// Upstream's `HtmlOverlayTests.zoomWalksTheLadder`.
#[test]
fn zoom_walks_the_ladder() {
    for (action, from, to) in [
        ("increase_font_size:1", 1.0, 1.15),
        ("increase_font_size:1", 2.5, 3.0),
        ("increase_font_size:1", 3.0, 3.0),
        ("increase_font_size:1", 1.1, 1.15),
        ("decrease_font_size:1", 1.0, 0.85),
        ("decrease_font_size:1", 0.5, 0.5),
        ("decrease_font_size:1", 1.1, 1.0),
        ("reset_font_size", 2.0, 1.0),
    ] {
        assert_eq!(zoom::applying(action, from), Some(to), "{action} {from}");
    }
}

/// Upstream's `HtmlOverlayTests.zoomIgnoresAnythingButAFontSizeAction`.
#[test]
fn zoom_ignores_anything_but_a_font_size_action() {
    assert_eq!(zoom::applying("paste_from_clipboard", 1.0), None);
}

/// Upstream's `HtmlOverlayTests.zoomResolvesAStoredFactorIntoRange`.
#[test]
#[expect(clippy::float_cmp, reason = "the bounds are exact literals")]
fn zoom_resolves_a_stored_factor_into_range() {
    for (stored, resolved) in [
        (None, 1.0),
        (Some(1.5), 1.5),
        (Some(0.1), 0.5),
        (Some(9.0), 3.0),
        (Some(f64::NAN), 1.0),
        (Some(f64::INFINITY), 1.0),
    ] {
        assert_eq!(zoom::resolve(stored), resolved, "{stored:?}");
    }
}
