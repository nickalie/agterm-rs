//! HTML overlays on `WebKitGTK` (upstream's `HtmlOverlayRegistry.swift`, `HtmlOverlayView.swift`, the handler side of
//! `HtmlOverlayBridge.swift` and the page parts of `WindowContentView+Detail.swift`).
//!
//! A registry owns every page's panel (the identity strip over the web view), keyed by the page's id rather than by
//! a host widget or a pane, so a page survives session switches, pane swaps and the soft-close window; it drops a page
//! only when the core releases it ([`settle`]). The session-wide page sits in the program layer's frame and a pane
//! page in its session's [`crate::split::PaneStack`] over the pane it covers, mounted by `crate::overlay`; a web view,
//! unlike a terminal's GL area, may change parent, which only a session moving to another window asks for.
//!
//! Every page is drawn in the terminal's colors and its scripts carry them as the page's theme; [`refresh_themes`]
//! brings them up to date after a config reload or theme change.
//!
//! The registry is the core's [`agterm_core::html::HtmlHost`] ([`install`]). Without the `html` cargo feature none is
//! installed, so the core refuses every page and nothing here mounts. Where `WebKit`'s sandbox cannot start, which
//! would end the app at the first web view, the host refuses every page instead (`sandbox`).

#[cfg(feature = "html")]
mod files;
#[cfg(feature = "html")]
mod page;
#[cfg(feature = "html")]
mod panel;
#[cfg(feature = "html")]
mod policy;
#[cfg(feature = "html")]
mod registry;
#[cfg(feature = "html")]
mod sandbox;

#[cfg(all(test, feature = "html"))]
pub(crate) use registry::on_screen;
#[cfg(feature = "html")]
pub use registry::{answer, apply, focus_target, install, mount, refresh_themes, settle};

#[cfg(not(feature = "html"))]
mod unavailable {
    //! The build without `WebKitGTK`: no host is installed, so no page ever reaches these.

    use agterm_core::Core;
    use agterm_core::html::HtmlOverlay;
    use uuid::Uuid;

    use crate::state::Poster;

    /// Installs nothing: the core refuses every page.
    pub fn install(_core: &mut Core, _state_dir: std::path::PathBuf) {}

    /// No page has a panel.
    #[must_use]
    pub fn mount(_overlay: &HtmlOverlay, _background: Option<&str>, _poster: &Poster) -> Option<gtk::Widget> {
        None
    }

    pub fn apply(_overlay: &HtmlOverlay, _on_screen: bool) {}

    #[must_use]
    pub fn focus_target(_id: Uuid) -> Option<gtk::Widget> {
        None
    }

    pub fn settle(_core: &mut Core) {}

    /// No page follows the theme.
    pub fn refresh_themes() {}
}

#[cfg(not(feature = "html"))]
pub use unavailable::{apply, focus_target, install, mount, refresh_themes, settle};
