//! Pages in the overlay slots (upstream's `AppStore+HtmlOverlay.swift`, `Session+HtmlOverlay.swift`): a page takes the
//! session-wide slot or one pane's slot with the slot rules of a program, answers a submit, reloads, and records what
//! the frontend's web view reports, wherever the page sits now, a session an undoable close holds included.
//!
//! A page has no terminal: it never counts as a program (`program_active`), only as a cover (`cover_overlay_active`).
//! Its outcome is the core's ([`crate::html::HtmlPageOutcomes`]): the core registers a page it opens, records a
//! submit's answer, and settles a page that left every slot as dismissed at the next commit, leaving an answered page
//! alone; closing a slot records nothing.

use uuid::Uuid;

use super::overlay::OverlayPane;
use super::pane::StatusPane;
use super::pane_overlay::PaneOverlay;
use super::session::Session;
use super::store::AppStore;
use super::window_library::WindowLibrary;
use crate::html::{HtmlLoadState, HtmlOverlay, HtmlPageInfo, HtmlReloadTarget};

/// Why [`AppStore::open_html_overlay`] refused (upstream's `HtmlOverlayOpenFailure`; its `presenter` case joins with
/// the remote session tasks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlOverlayOpenFailure {
    UnknownSession,
    AlreadyOpen,
    PaneNotVisible,
}

/// Why a command addressing a page refused (upstream's `HtmlOverlayCommandFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlOverlayCommandFailure {
    UnknownSession,
    NoOverlay,
    NotHtml,
}

impl HtmlOverlayCommandFailure {
    /// The control error for this refusal (upstream's `message`).
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::UnknownSession => "no such session",
            Self::NoOverlay => "no overlay",
            Self::NotHtml => "the overlay is not an html page",
        }
    }
}

/// Where a page sits: its window, its session, and the pane slot holding it (`None` for the session-wide one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlPageSlot {
    pub window: Uuid,
    pub session: Uuid,
    pub pane: Option<OverlayPane>,
}

impl Session {
    /// Whether a page covers the session (upstream's `htmlOverlayActive`).
    #[must_use]
    pub fn html_overlay_active(&self) -> bool {
        self.overlay.html_active()
    }

    /// Whether `pane`'s own overlay is a page (upstream's `paneOverlayIsHtml`).
    #[must_use]
    pub fn pane_overlay_is_html(&self, pane: OverlayPane) -> bool {
        self.pane_overlay(pane).is_some_and(|overlay| overlay.html.is_some())
    }

    /// Whether a page holds the slot a `--pane` command addresses, the session-wide one for `None` (upstream's
    /// `htmlCovers`).
    #[must_use]
    pub fn html_covers(&self, pane: Option<OverlayPane>) -> bool {
        pane.map_or_else(|| self.html_overlay_active(), |pane| self.pane_overlay_is_html(pane))
    }

    /// Whether a page covers the terminal a `--pane` font command addresses (upstream's `htmlHidesTerminal`): the
    /// session-wide page covers both panes and a shown scratch, a pane page only its own pane.
    #[must_use]
    pub fn html_hides_terminal(&self, pane: Option<StatusPane>) -> bool {
        match pane {
            Some(StatusPane::Scratch) => self.html_overlay_active() && self.scratch_active,
            None | Some(StatusPane::Left) => self.html_overlay_active() || self.pane_overlay_is_html(OverlayPane::Left),
            Some(StatusPane::Right) => self.html_overlay_active() || self.pane_overlay_is_html(OverlayPane::Right),
        }
    }

    /// The page that takes the keyboard when focus returns to this session, in the order covers resolve; `None` when a
    /// terminal is on top (upstream's `topmostHtmlOverlay`).
    #[must_use]
    pub fn topmost_html_overlay(&self) -> Option<&HtmlOverlay> {
        if let Some(page) = self.overlay.page() {
            return Some(page);
        }
        if self.overlay.program_active() || self.scratch_active {
            return None;
        }
        self.focused_overlay_pane()
            .and_then(|pane| self.pane_overlay(pane))
            .and_then(|overlay| overlay.html.as_ref())
    }

    /// The pages in this session's slots, session-wide first, then left and right, each with its slot's pane.
    #[must_use]
    pub fn html_overlays(&self) -> Vec<(Option<OverlayPane>, &HtmlOverlay)> {
        let mut pages: Vec<(Option<OverlayPane>, &HtmlOverlay)> =
            self.overlay.page().map(|page| (None, page)).into_iter().collect();
        for pane in [OverlayPane::Left, OverlayPane::Right] {
            if let Some(page) = self.pane_overlay(pane).and_then(|overlay| overlay.html.as_ref()) {
                pages.push((Some(pane), page));
            }
        }
        pages
    }

    /// The slot holding page `id` here: `Some(None)` for the session-wide one.
    #[must_use]
    pub fn html_overlay_pane(&self, id: Uuid) -> Option<Option<OverlayPane>> {
        self.html_overlays()
            .into_iter()
            .find(|(_, page)| page.id == id)
            .map(|(pane, _)| pane)
    }

    /// The page in the slot `pane` addresses (upstream's private `htmlOverlay(_:pane:)`).
    ///
    /// # Errors
    ///
    /// No program or page holds the slot, or a program does.
    pub fn html_overlay_in(&self, pane: Option<OverlayPane>) -> Result<&HtmlOverlay, HtmlOverlayCommandFailure> {
        let occupied = match pane {
            Some(pane) => self.pane_overlay(pane).is_some(),
            None => self.overlay.active && !self.overlay.hud_active(),
        };
        if !occupied {
            return Err(HtmlOverlayCommandFailure::NoOverlay);
        }
        let shown = match pane {
            Some(pane) => self.pane_overlay(pane).and_then(|overlay| overlay.html.as_ref()),
            None => self.overlay.html.as_ref(),
        };
        shown.ok_or(HtmlOverlayCommandFailure::NotHtml)
    }

    /// Applies `change` to page `id` in whichever slot of this session holds it; false when none does.
    pub fn update_html_overlay(&mut self, id: Uuid, change: impl FnOnce(&mut HtmlOverlay)) -> bool {
        if let Some(page) = self.overlay.html.as_mut().filter(|page| page.id == id) {
            change(page);
            return true;
        }
        for pane in [OverlayPane::Left, OverlayPane::Right] {
            let page = self
                .pane_overlays
                .slot_mut(pane)
                .overlay
                .as_mut()
                .and_then(|overlay| overlay.html.as_mut())
                .filter(|page| page.id == id);
            if let Some(page) = page {
                change(page);
                return true;
            }
        }
        false
    }
}

impl AppStore {
    /// Opens a page in the session-wide slot (`pane` `None`) or one pane's slot, with the slot rules of
    /// [`AppStore::open_overlay`]/[`AppStore::open_pane_overlay`] (upstream's `openHtmlOverlay`): a HUD yields the
    /// session-wide slot, a program or page refuses; a pane slot needs its pane laid out. The size is clamped into
    /// 1...100. The caller registers the page's outcome.
    ///
    /// # Errors
    ///
    /// Why it refused, with nothing changed.
    pub fn open_html_overlay(
        &mut self,
        id: Uuid,
        pane: Option<OverlayPane>,
        overlay: HtmlOverlay,
        size_percent: Option<i64>,
        background_color: Option<String>,
    ) -> Result<(), HtmlOverlayOpenFailure> {
        let session = self.session_mut(id).ok_or(HtmlOverlayOpenFailure::UnknownSession)?;
        if let Some(pane) = pane {
            if session.pane_overlay(pane).is_some() {
                return Err(HtmlOverlayOpenFailure::AlreadyOpen);
            }
            if !session.renders_overlay_pane(pane) {
                return Err(HtmlOverlayOpenFailure::PaneNotVisible);
            }
            let generation = session.pane_overlays.next_generation();
            let slot = session.pane_overlays.slot_mut(pane);
            slot.exit_code = None;
            slot.overlay = Some(PaneOverlay {
                command: String::new(),
                cwd: None,
                background_color,
                wait: false,
                generation,
                code_file: None,
                html: Some(overlay),
            });
            return Ok(());
        }
        let slot = &mut session.overlay;
        if slot.hud_active() {
            slot.close();
        }
        if slot.active {
            return Err(HtmlOverlayOpenFailure::AlreadyOpen);
        }
        slot.generation += 1;
        slot.exit_code = None;
        slot.code_file = None;
        slot.command = None;
        slot.cwd = None;
        slot.wait = false;
        slot.size_percent = size_percent.map(|percent| percent.clamp(1, 100));
        slot.background_color = background_color;
        slot.html = Some(overlay);
        slot.active = true;
        Ok(())
    }

    /// The page in the slot `pane` of session `id` addresses (upstream's `htmlOverlayCommandFailure` when it fails).
    ///
    /// # Errors
    ///
    /// An unknown session, an empty slot, or a slot holding a program.
    pub fn html_overlay(&self, id: Uuid, pane: Option<OverlayPane>) -> Result<&HtmlOverlay, HtmlOverlayCommandFailure> {
        self.session(id)
            .ok_or(HtmlOverlayCommandFailure::UnknownSession)?
            .html_overlay_in(pane)
    }

    /// Asks the frontend to reload the page, keeping the slot, its identity and the web view (upstream's
    /// `reloadHtmlOverlay`): the revision moves, the page is loading again, and a reload of the original forgets
    /// where the user had navigated.
    ///
    /// # Errors
    ///
    /// As [`AppStore::html_overlay`].
    pub fn reload_html_overlay(
        &mut self,
        id: Uuid,
        pane: Option<OverlayPane>,
        target: HtmlReloadTarget,
    ) -> Result<(), HtmlOverlayCommandFailure> {
        let page_id = self.html_overlay(id, pane)?.id;
        if let Some(session) = self.session_mut(id) {
            session.update_html_overlay(page_id, |page| reload(page, target));
        }
        Ok(())
    }
}

/// One reload of `page` (upstream's `reloadHtmlOverlay` change).
fn reload(page: &mut HtmlOverlay, target: HtmlReloadTarget) {
    page.reload_revision += 1;
    page.reload_target = target;
    page.load_state = HtmlLoadState::Loading;
    page.load_error = None;
    if target == HtmlReloadTarget::Original {
        page.current = None;
    }
}

impl WindowLibrary {
    /// Where page `id` sits in a visible session of an open window (upstream's `htmlOverlaySlot`); `None` once the
    /// page left its slot, and while its session sits in an undoable close.
    #[must_use]
    pub fn html_overlay_slot(&self, id: Uuid) -> Option<HtmlPageSlot> {
        self.open_ids().into_iter().find_map(|window| {
            self.store(window)?.sessions().find_map(|session| {
                session.html_overlay_pane(id).map(|pane| HtmlPageSlot {
                    window,
                    session: session.id(),
                    pane,
                })
            })
        })
    }

    /// The ids of every page an open window holds, in its sessions and in its undoable closes, whose web views go on.
    #[must_use]
    pub fn live_html_overlays(&self) -> Vec<Uuid> {
        let mut pages = Vec::new();
        for window in self.open_ids() {
            let Some(store) = self.store(window) else { continue };
            let pending = store.pending_close_members();
            let sessions = store.sessions().chain(pending.iter().map(|member| member.session));
            pages.extend(sessions.flat_map(|session| session.html_overlays().into_iter().map(|(_, page)| page.id)));
        }
        pages
    }

    /// Applies `change` to page `id` wherever its slot now is, a session an undoable close holds included (upstream's
    /// `updateHtmlOverlay`); false once the page left its slot.
    pub fn update_html_overlay(&mut self, id: Uuid, change: impl FnOnce(&mut HtmlOverlay)) -> bool {
        let Some(session) = self.session_holding_page(id) else {
            return false;
        };
        self.held_session_mut(session)
            .is_some_and(|session| session.update_html_overlay(id, change))
    }

    /// Records what the web view reported for page `id` (upstream's `setHtmlLoadState`); a no-op once it left its slot.
    pub fn set_html_load_state(&mut self, id: Uuid, state: HtmlLoadState, error: Option<String>) -> bool {
        self.update_html_overlay(id, |page| {
            page.load_state = state;
            page.load_error = error;
        })
    }

    /// Records what the web view shows now (upstream's `setHtmlPage`).
    pub fn set_html_page(&mut self, id: Uuid, info: Option<HtmlPageInfo>) -> bool {
        self.update_html_overlay(id, |page| page.current = info)
    }

    /// Reloads page `id` wherever it sits (the toolbar's reload; upstream's `HtmlOverlayRegistry.reload(_:target:)`).
    pub fn reload_html_overlay_by_id(&mut self, id: Uuid, target: HtmlReloadTarget) -> bool {
        let Some(slot) = self.html_overlay_slot(id) else {
            return false;
        };
        self.store_mut(slot.window)
            .is_some_and(|store| store.reload_html_overlay(slot.session, slot.pane, target).is_ok())
    }

    /// Closes page `id` wherever its slot now is (upstream's `closeHtmlOverlay`): the toolbar's close button and a
    /// page's own `window.close()`, whose view cannot trust the pane it was built for after a swap. False while its
    /// session sits in an undoable close or once it left its slot.
    pub fn close_html_overlay(&mut self, id: Uuid) -> bool {
        let Some(slot) = self.html_overlay_slot(id) else {
            return false;
        };
        let Some(store) = self.store_mut(slot.window) else {
            return false;
        };
        match slot.pane {
            Some(pane) => store.close_pane_overlay(slot.session, pane),
            None => store.close_overlay(slot.session),
        }
    }

    /// The session holding page `id` in an open window, visible or in an undoable close.
    fn session_holding_page(&self, id: Uuid) -> Option<Uuid> {
        self.open_ids().into_iter().find_map(|window| {
            let store = self.store(window)?;
            let pending = store.pending_close_members();
            let mut sessions = store.sessions().chain(pending.iter().map(|member| member.session));
            sessions
                .find(|session| session.html_overlay_pane(id).is_some())
                .map(Session::id)
        })
    }
}
