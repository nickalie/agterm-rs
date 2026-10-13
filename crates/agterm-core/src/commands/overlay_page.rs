//! `session.overlay.*` against an HTML page (upstream's page arms of `ControlServer+SessionActions.swift`:
//! `openHtmlOverlay`, `submitSessionOverlay`, `reloadSessionOverlay`, `navigateSessionOverlay`, `htmlPageResult`), and
//! the core's side of the frontend's web views: what they report, the page bridge's entry, and the pages released
//! from every slot (upstream's `HtmlOverlayReleases`).
//!
//! A page's outcome lives here, keyed by its id ([`HtmlPageOutcomes`]): an open registers it, a submit records the
//! value before the close, and a page found gone from every slot of every open window (its session's undoable close
//! included) is dismissed and handed to the frontend to release its web view, whatever path emptied the slot.

use std::collections::HashSet;
use std::fmt;

use agterm_proto::{Args, ControlResult, Request};
use url::Url;
use uuid::Uuid;

use super::{Outcome, session_store, with_id};
use crate::Core;
use crate::error::ControlError;
use crate::html::bridge::{self, HtmlBridgePage};
use crate::html::{
    HtmlHost, HtmlLoadState, HtmlNavigation, HtmlOverlay, HtmlPageFlags, HtmlPageInfo, HtmlPageOutcomes,
    HtmlReloadTarget, HtmlSource, errors, zoom,
};
use crate::model::{HtmlOverlayOpenFailure, HtmlPageSlot, OverlayPane, StatusPane};
use crate::pane_env::uuid_string;

/// A page that passed validation in a build with no web views (the `html` feature off).
pub(crate) const PAGES_UNAVAILABLE: &str = "session.overlay.open: html overlays are not available in this build";
/// Upstream's `OverlayHtmlError.noResult`/`noRead`: a page runs no program, so there is no status and no text.
pub(crate) const PAGE_NO_RESULT: &str = "no overlay result: the slot holds an html page";
pub(crate) const PAGE_NO_READ: &str = "no overlay to read: the slot holds an html page";
const UNKNOWN_PAGE: &str = "no such page";
const ALREADY_OPEN: &str = "overlay already open";
const PANE_ALREADY_OPEN: &str = "pane overlay already open";
/// The bridge's refusals of a page that left its slot (upstream's `handleBridgeRequest`).
const PAGE_CLOSED: &str = "page closed";

/// The pages' outcomes, the pages the frontend may still hold a web view for, and the frontend's web views.
#[derive(Default)]
pub(crate) struct PageRuntime {
    outcomes: HtmlPageOutcomes,
    /// Every page opened and not yet released.
    known: HashSet<Uuid>,
    /// Pages that left every slot since the frontend last took them.
    released: Vec<Uuid>,
    host: Option<Box<dyn HtmlHost>>,
}

impl fmt::Debug for PageRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageRuntime")
            .field("outcomes", &self.outcomes)
            .field("known", &self.known)
            .field("released", &self.released)
            .field("host", &self.host.is_some())
            .finish()
    }
}

/// How a page opens: what `session.overlay.open` validated, or a clicked link's browsing page.
pub(crate) struct PageOpening {
    pub source: HtmlSource,
    pub flags: HtmlPageFlags,
    pub pane: Option<OverlayPane>,
    pub size_percent: Option<i64>,
    pub background_color: Option<String>,
    pub follow: bool,
}

impl PageOpening {
    /// The page `args` describe in the slot `pane`.
    pub(crate) fn from_args(source: HtmlSource, args: &Args, pane: Option<OverlayPane>) -> Self {
        let set = |flag: Option<bool>| flag == Some(true);
        Self {
            source,
            flags: HtmlPageFlags {
                navigation: set(args.navigation),
                javascript: set(args.javascript),
                chromeless: set(args.chromeless),
                persistent: set(args.persistent),
                browse: set(args.browse),
            },
            pane,
            size_percent: args.size_percent,
            background_color: args.color.clone(),
            follow: set(args.follow),
        }
    }
}

/// `session.overlay.open --html|--url` once validated (upstream's `openHtmlOverlay`): the page takes its slot with the
/// slot rules of a program and answers with its id, the one `session.overlay.result --page` reads.
pub(crate) fn open(core: &mut Core, target: Option<&str>, window: Option<&str>, opening: PageOpening) -> Outcome {
    let id = session_store(core, target, window)?.1;
    let Some(host) = core.pages.host.as_ref() else {
        return Err(ControlError::rejected(PAGES_UNAVAILABLE));
    };
    if opening.flags.persistent
        && let Some(failure) = host.persistent_store_failure()
    {
        return Err(ControlError::rejected(format!("session.overlay.open: {failure}")));
    }
    let page = HtmlOverlay::new(opening.source, opening.flags);
    let page_id = page.id;
    let (store, _) = session_store(core, target, window)?;
    store
        .open_html_overlay(id, opening.pane, page, opening.size_percent, opening.background_color)
        .map_err(|failure| ControlError::rejected(open_failure(failure, opening.pane)))?;
    if opening.follow {
        store.select_session(Some(id));
    }
    core.pages.outcomes.register(page_id);
    core.pages.known.insert(page_id);
    Ok(ControlResult {
        page_id: Some(uuid_string(page_id)),
        ..with_id(id)
    })
}

/// The error of a refused page open (upstream's `HtmlOverlayOpenFailure.message(pane:)`).
fn open_failure(failure: HtmlOverlayOpenFailure, pane: Option<OverlayPane>) -> &'static str {
    match failure {
        HtmlOverlayOpenFailure::UnknownSession => "no such session",
        HtmlOverlayOpenFailure::AlreadyOpen if pane.is_none() => ALREADY_OPEN,
        HtmlOverlayOpenFailure::AlreadyOpen => PANE_ALREADY_OPEN,
        HtmlOverlayOpenFailure::PaneNotVisible => super::hud::PANE_NOT_VISIBLE,
    }
}

/// `session.overlay.submit` (upstream's `submitHtmlOverlay`): the page in the addressed slot is closed and answered
/// with `value`. The close only empties the slot and records nothing; the page is settled as dismissed later, at the
/// next commit's `Core::sync_pages` (or a `result --page` read), which leaves a page that already has an answer alone,
/// so the answer recorded here stands.
pub(crate) fn submit(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    pane: Option<OverlayPane>,
    value: &str,
) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let page_id = store
        .html_overlay(id, pane)
        .map_err(|failure| ControlError::rejected(failure.message()))?
        .id;
    match pane {
        Some(pane) => store.close_pane_overlay(id, pane),
        None => store.close_overlay(id),
    };
    core.pages.outcomes.submit(page_id, value);
    Ok(with_id(id))
}

/// `session.overlay.reload` (upstream's `reloadSessionOverlay`): the original source, or with `--current` the page
/// the user navigated to; the frontend reloads at its next render, the web view kept.
pub(crate) fn reload(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    pane: Option<OverlayPane>,
    current: bool,
) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let reload = if current {
        HtmlReloadTarget::Current
    } else {
        HtmlReloadTarget::Original
    };
    store
        .reload_html_overlay(id, pane, reload)
        .map_err(|failure| ControlError::rejected(failure.message()))?;
    Ok(with_id(id))
}

/// `session.overlay.navigate` (upstream's `navigateSessionOverlay`): a history step or hand-off on the page's web
/// view, refused while it has none.
pub(crate) fn navigate(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    pane: Option<OverlayPane>,
    navigation: HtmlNavigation,
) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let page_id = store
        .html_overlay(id, pane)
        .map_err(|failure| ControlError::rejected(failure.message()))?
        .id;
    let host = core
        .pages
        .host
        .as_ref()
        .ok_or_else(|| ControlError::rejected(errors::NOT_REALIZED))?;
    host.navigate(page_id, navigation).map_err(ControlError::rejected)?;
    Ok(with_id(id))
}

/// `session.overlay.result --page` (upstream's `htmlPageResult`): where the page stands, by its id alone.
pub(crate) fn page_result(core: &mut Core, page: Uuid) -> Outcome {
    core.sync_pages();
    let outcome = core
        .pages
        .outcomes
        .outcome(page)
        .ok_or_else(|| ControlError::rejected(UNKNOWN_PAGE))?;
    Ok(ControlResult {
        page_outcome: Some(outcome),
        ..ControlResult::default()
    })
}

// MARK: - the frontend's hooks

impl Core {
    /// Installs the frontend's web views; without them a page that passed validation is refused.
    pub fn set_html_host(&mut self, host: Box<dyn HtmlHost>) {
        self.pages.host = Some(host);
    }

    /// The frontend's web views, if it installed them.
    pub(crate) fn html_host(&self) -> Option<&dyn HtmlHost> {
        self.pages.host.as_deref()
    }

    /// Settles the pages that left every slot: each is dismissed (a no-op for one already answered) and queued for
    /// the frontend to release its web view. Runs at every commit and before an outcome is read.
    pub(crate) fn sync_pages(&mut self) {
        let live: HashSet<Uuid> = self.library().live_html_overlays().into_iter().collect();
        let gone: Vec<Uuid> = self.pages.known.difference(&live).copied().collect();
        for page in gone {
            self.pages.known.remove(&page);
            self.pages.outcomes.dismiss(page);
            self.pages.released.push(page);
        }
    }

    /// Takes the pages released since the last take: their web views go.
    pub fn take_released_pages(&mut self) -> Vec<Uuid> {
        std::mem::take(&mut self.pages.released)
    }

    /// Where page `id` sits in a visible session of an open window.
    #[must_use]
    pub fn html_overlay_slot(&self, id: Uuid) -> Option<HtmlPageSlot> {
        self.library().html_overlay_slot(id)
    }

    /// The page zoom every page shows at (upstream's `HtmlOverlayRegistry.zoom`, from `htmlOverlayZoom`).
    #[must_use]
    pub fn html_zoom(&self) -> f64 {
        zoom::resolve(self.settings().html_overlay_zoom)
    }

    /// The web view of page `id` reported its load state; whether the page still sits in a slot. Commit afterwards.
    pub fn set_html_load_state(&mut self, id: Uuid, state: HtmlLoadState, error: Option<String>) -> bool {
        self.library_mut().set_html_load_state(id, state, error)
    }

    /// The web view of page `id` reported what it shows; whether the page still sits in a slot.
    pub fn set_html_page(&mut self, id: Uuid, info: Option<HtmlPageInfo>) -> bool {
        self.library_mut().set_html_page(id, info)
    }

    /// The toolbar's reload of page `id`, wherever it sits.
    pub fn reload_html_overlay(&mut self, id: Uuid, target: HtmlReloadTarget) -> bool {
        self.library_mut().reload_html_overlay_by_id(id, target)
    }

    /// Closes page `id` wherever it sits (the strip's close button, the page's own `window.close()`); false while its
    /// session sits in an undoable close.
    pub fn close_html_overlay(&mut self, id: Uuid) -> bool {
        self.library_mut().close_html_overlay(id)
    }

    /// The user focused pane page `id`: the split's focus follows the pane it covers (upstream's `onFocus`).
    pub fn html_page_focused(&mut self, id: Uuid) -> bool {
        let Some(HtmlPageSlot {
            window,
            session,
            pane: Some(pane),
        }) = self.html_overlay_slot(id)
        else {
            return false;
        };
        self.library_mut()
            .store_mut(window)
            .is_some_and(|store| store.set_split_focus(session, pane == OverlayPane::Right))
    }

    /// The user clicked session-wide page `id` while a pane ask is up beside it: the split's focus moves to the pane
    /// the ask leaves uncovered, or the next refocus hands the keys back to the ask (upstream's `onClick`).
    pub fn html_page_clicked(&mut self, id: Uuid) -> bool {
        let Some(HtmlPageSlot {
            window,
            session,
            pane: None,
        }) = self.html_overlay_slot(id)
        else {
            return false;
        };
        let Some(store) = self.library_mut().store_mut(window) else {
            return false;
        };
        let Some(ask) = store.session(session).and_then(crate::model::Session::ask_target_pane) else {
            return false;
        };
        store.set_split_focus(session, ask == OverlayPane::Left)
    }

    /// The request page `id`'s message makes (upstream's `handleBridgeRequest`): resolved where the page sits now, so
    /// a request after a swap or a move acts from its new place, and a page that left its slot is refused.
    ///
    /// # Errors
    ///
    /// The message the page's promise rejects with.
    pub fn html_bridge_request(&self, id: Uuid, body: &[u8]) -> Result<Request, String> {
        let slot = self.html_overlay_slot(id).ok_or_else(|| PAGE_CLOSED.to_owned())?;
        let page = HtmlBridgePage {
            window: Some(uuid_string(slot.window)),
            session: slot.session,
            pane: slot.pane,
        };
        bridge::request(body, &page).map_err(|error| error.message)
    }

    /// Shows a clicked link as a browsing page over its session (upstream's `openLinkOverlay`), and says whether it
    /// did; the caller opens the link in the browser otherwise. A HUD on the session and a zoomed terminal in its window
    /// refuse it up front: the page would close the HUD for good, or show nowhere.
    pub fn open_link_overlay(&mut self, url: Url, session: Uuid) -> bool {
        let Some(found) = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
        else {
            return false;
        };
        if found.overlay.hud_active() {
            return false;
        }
        if self
            .library()
            .window_id_for_session(session)
            .is_some_and(|window| self.zoom_target(window).is_some())
        {
            return false;
        }
        let opening = PageOpening {
            source: HtmlSource::Url(url),
            flags: HtmlPageFlags {
                navigation: true,
                javascript: true,
                chromeless: false,
                persistent: true,
                browse: true,
            },
            pane: None,
            size_percent: None,
            background_color: None,
            follow: false,
        };
        open(self, Some(&uuid_string(session)), None, opening).is_ok()
    }

    /// Whether a page covers the terminal a font command addresses in session `session` (upstream's
    /// `htmlHidesTerminal`): the command then walks the page zoom instead.
    #[must_use]
    pub fn html_hides_terminal(&self, session: Uuid, pane: Option<StatusPane>) -> bool {
        self.library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
            .is_some_and(|found| found.html_hides_terminal(pane))
    }
}
