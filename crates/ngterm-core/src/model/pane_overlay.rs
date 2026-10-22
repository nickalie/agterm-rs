//! The pane-scoped overlays: one per split pane, each covering that pane alone and leaving its sibling live (upstream's
//! `PaneOverlay`, the `leftOverlay*`/`rightOverlay*` fields of `Session.swift` and `openPaneOverlay`,
//! `closePaneOverlay`, `teardownPaneOverlay`, `promotePaneOverlay`, `dropUnrealizedPaneOverlays`).
//!
//! An overlay follows its pane: a swap exchanges them, a promotion moves the survivor's into the left slot with its
//! exit status, and a pane that goes takes its overlay with it. Every overlay carries a generation unique within its
//! session, which the frontend keys its terminal by, so a moved overlay keeps its terminal. Ephemeral: never persisted.

use std::path::PathBuf;

use uuid::Uuid;

use super::overlay::{OverlayPane, OverlayProgram};
use super::pane::SurfaceSlot;
use super::session::Session;
use super::store::AppStore;
use crate::html::HtmlOverlay;
use crate::overlay_capture::{mint_code_file, take_exit_code};

/// One pane's overlay (upstream's `PaneOverlay`): the session-wide slot's program fields minus the size, as a pane
/// overlay is always full-pane, or a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOverlay {
    /// The program; empty for a page, and never read then.
    pub command: String,
    /// The working directory; `None` inherits the session's.
    pub cwd: Option<String>,
    /// The overlay's own `#rrggbb` background.
    pub background_color: Option<String>,
    /// Hold the terminal after the program exits.
    pub wait: bool,
    /// Unique within the session, minted at the open; the frontend keys the overlay's terminal by it.
    pub generation: u64,
    /// Where the wrapper writes the program's exit status, read when the terminal is torn down; `None` for a page.
    pub code_file: Option<PathBuf>,
    /// The page this overlay shows instead of running `command` (upstream's `PaneOverlay.html`).
    pub html: Option<HtmlOverlay>,
}

impl PaneOverlay {
    /// Whether this overlay runs a program, which a terminal hosts: not a page.
    #[must_use]
    pub fn is_program(&self) -> bool {
        self.html.is_none()
    }
}

/// One pane's overlay slot: the overlay, its terminal, and the last exit status, which outlives the overlay's close.
#[derive(Debug, Default)]
pub struct PaneOverlaySlot {
    pub overlay: Option<PaneOverlay>,
    pub surface: SurfaceSlot,
    /// The last program's exit status, cleared by the next open on this pane (upstream keeps it outside the overlay
    /// so `session.overlay.result --pane` reads it after the close).
    pub exit_code: Option<i32>,
}

impl PaneOverlaySlot {
    /// Closes the overlay: its terminal torn down, the program's status recorded from its wrapper's file. The exit
    /// status survives (upstream's `closePaneOverlay`). False with no overlay.
    pub(crate) fn close(&mut self) -> bool {
        let Some(overlay) = self.overlay.take() else {
            return false;
        };
        self.surface.teardown();
        if let Some(code) = overlay.code_file.as_deref().and_then(take_exit_code) {
            self.exit_code = Some(code);
        }
        true
    }

    /// Frees the slot outright, the exit status too: the pane itself goes (upstream's `teardownPaneOverlay`).
    fn teardown(&mut self) {
        self.close();
        self.surface.teardown();
        self.exit_code = None;
    }
}

/// A session's two pane overlay slots.
#[derive(Debug, Default)]
pub struct PaneOverlays {
    pub left: PaneOverlaySlot,
    pub right: PaneOverlaySlot,
    /// The last generation minted.
    generation: u64,
}

impl PaneOverlays {
    /// The slot of `pane`.
    #[must_use]
    pub fn slot(&self, pane: OverlayPane) -> &PaneOverlaySlot {
        match pane {
            OverlayPane::Left => &self.left,
            OverlayPane::Right => &self.right,
        }
    }

    /// The slot of `pane`, for mutation.
    pub fn slot_mut(&mut self, pane: OverlayPane) -> &mut PaneOverlaySlot {
        match pane {
            OverlayPane::Left => &mut self.left,
            OverlayPane::Right => &mut self.right,
        }
    }

    /// The overlay on `pane`.
    #[must_use]
    pub fn get(&self, pane: OverlayPane) -> Option<&PaneOverlay> {
        self.slot(pane).overlay.as_ref()
    }

    /// The panes with an overlay up, left then right (upstream's `openPaneOverlays`).
    #[must_use]
    pub fn open_panes(&self) -> Vec<OverlayPane> {
        [OverlayPane::Left, OverlayPane::Right]
            .into_iter()
            .filter(|pane| self.get(*pane).is_some())
            .collect()
    }

    /// The pane whose overlay carries `generation` now, following swaps and promotions.
    #[must_use]
    pub fn pane_of(&self, generation: u64) -> Option<OverlayPane> {
        [OverlayPane::Left, OverlayPane::Right]
            .into_iter()
            .find(|pane| self.get(*pane).is_some_and(|overlay| overlay.generation == generation))
    }

    /// Frees `pane`'s overlay outright, the exit status included: the pane goes.
    pub fn teardown(&mut self, pane: OverlayPane) {
        self.slot_mut(pane).teardown();
    }

    /// Frees both slots (upstream's `teardownPaneOverlays`).
    pub fn teardown_all(&mut self) {
        self.left.teardown();
        self.right.teardown();
    }

    /// Moves the right slot, overlay, terminal and exit status together, into the left one, following a split
    /// survivor promoted into the primary pane (upstream's `promotePaneOverlay`). The left slot must be freed first.
    pub fn promote(&mut self) {
        self.left = std::mem::take(&mut self.right);
    }

    /// Exchanges the slots, following a pane swap.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.left, &mut self.right);
    }

    pub(super) fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
}

/// Why [`AppStore::open_pane_overlay`] refused (upstream's `PaneOverlayOpenFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneOverlayOpenFailure {
    UnknownSession,
    AlreadyOpen,
    /// The detail pane does not lay the pane out, so its terminal would never come up.
    PaneNotVisible,
}

impl Session {
    /// The overlay on `pane` (upstream's `paneOverlay(_:)`).
    #[must_use]
    pub fn pane_overlay(&self, pane: OverlayPane) -> Option<&PaneOverlay> {
        self.pane_overlays.get(pane)
    }

    /// The focused pane as an overlay pane (upstream's `focusedPane`).
    #[must_use]
    pub fn focused_overlay_pane_role(&self) -> OverlayPane {
        if self.focuses_split() {
            OverlayPane::Right
        } else {
            OverlayPane::Left
        }
    }

    /// The focused pane, when its overlay slot is taken (upstream's `focusedOverlayPane`).
    #[must_use]
    pub fn focused_overlay_pane(&self) -> Option<OverlayPane> {
        let pane = self.focused_overlay_pane_role();
        self.pane_overlay(pane).is_some().then_some(pane)
    }

    /// Whether a caller's program or page covers the session and owns its input (upstream's `coverOverlayActive`), the
    /// input-exclusion question; terminal-surface questions ask [`super::OverlaySlot::program_active`].
    #[must_use]
    pub fn cover_overlay_active(&self) -> bool {
        self.overlay.program_active() || self.overlay.html_active()
    }

    /// Whether a program or page covers the whole session, no size given (upstream's `fullOverlayActive`): it hides
    /// the panes and a shown scratch. A HUD never does.
    #[must_use]
    pub fn full_overlay_active(&self) -> bool {
        self.cover_overlay_active() && self.overlay.size_percent.is_none()
    }

    /// Drops the pane overlays that can never come up (upstream's `dropUnrealizedPaneOverlays`): no host lays their
    /// pane out (`hosted` says whether something else, a zoom, claims it) and their terminal never realized, so no
    /// program started and nothing would ever close the slot. A realized one is left alone: its program runs on
    /// unseen. Whether any went.
    pub fn drop_unrealized_pane_overlays(&mut self, hosted: impl Fn(OverlayPane) -> bool) -> bool {
        let mut dropped = false;
        for pane in [OverlayPane::Left, OverlayPane::Right] {
            let slot = self.pane_overlays.slot(pane);
            let stranded = slot.overlay.as_ref().is_some_and(PaneOverlay::is_program)
                && !slot.surface.get().is_some_and(ngterm_host::Surface::is_realized)
                && !self.renders_overlay_pane(pane)
                && !hosted(pane);
            if stranded {
                self.pane_overlays.teardown(pane);
                dropped = true;
            }
        }
        dropped
    }
}

impl AppStore {
    /// Opens an overlay covering `pane` alone (upstream's `openPaneOverlay`): like the session-wide one but always
    /// full-pane, its `cwd`, `wait` and background its own. Refused for an occupied slot and for a pane the detail
    /// pane does not lay out, whose terminal would never come up. `program.size_percent` is ignored.
    ///
    /// # Errors
    ///
    /// Why it refused, with nothing changed.
    pub fn open_pane_overlay(
        &mut self,
        id: Uuid,
        pane: OverlayPane,
        program: OverlayProgram,
    ) -> Result<(), PaneOverlayOpenFailure> {
        let session = self.session_mut(id).ok_or(PaneOverlayOpenFailure::UnknownSession)?;
        if session.pane_overlay(pane).is_some() {
            return Err(PaneOverlayOpenFailure::AlreadyOpen);
        }
        if !session.renders_overlay_pane(pane) {
            return Err(PaneOverlayOpenFailure::PaneNotVisible);
        }
        let generation = session.pane_overlays.next_generation();
        let slot = session.pane_overlays.slot_mut(pane);
        slot.exit_code = None;
        slot.overlay = Some(PaneOverlay {
            command: program.command,
            cwd: program.cwd,
            background_color: program.background_color,
            wait: program.wait,
            generation,
            code_file: Some(mint_code_file()),
            html: None,
        });
        session.remote_overlays.clear_failure(Some(pane));
        Ok(())
    }

    /// Closes `pane`'s overlay (upstream's `closePaneOverlay`): its terminal torn down, the exit status kept. False with
    /// no overlay there.
    pub fn close_pane_overlay(&mut self, id: Uuid, pane: OverlayPane) -> bool {
        self.session_mut(id)
            .is_some_and(|session| session.pane_overlays.slot_mut(pane).close())
    }
}

#[cfg(test)]
mod tests {
    use ngterm_host::fakes::FakeSurface;

    use super::*;
    use crate::model::NewSession;
    use crate::model::SplitLayout;

    fn store_with_session() -> (AppStore, Uuid) {
        let mut store = AppStore::new();
        let workspace = store.add_workspace("w");
        let id = store.add_session(workspace, NewSession::at_cwd("/a")).unwrap();
        (store, id)
    }

    fn program(command: &str) -> OverlayProgram {
        OverlayProgram {
            command: command.to_owned(),
            ..OverlayProgram::default()
        }
    }

    fn split(store: &mut AppStore, id: Uuid) {
        let session = store.session_mut(id).unwrap();
        session.split = SplitLayout {
            shown: true,
            exists: true,
            focused: true,
            ..SplitLayout::default()
        };
        session.split_pane_identity = Some(Uuid::new_v4());
    }

    #[test]
    fn a_pane_overlay_needs_a_laid_out_free_pane() {
        let (mut store, id) = store_with_session();
        assert_eq!(
            store.open_pane_overlay(id, OverlayPane::Right, program("cat")),
            Err(PaneOverlayOpenFailure::PaneNotVisible)
        );
        assert_eq!(store.open_pane_overlay(id, OverlayPane::Left, program("cat")), Ok(()));
        assert_eq!(
            store.open_pane_overlay(id, OverlayPane::Left, program("cat")),
            Err(PaneOverlayOpenFailure::AlreadyOpen)
        );
        assert_eq!(
            store.open_pane_overlay(Uuid::new_v4(), OverlayPane::Left, program("cat")),
            Err(PaneOverlayOpenFailure::UnknownSession)
        );
        assert_eq!(
            store.session(id).unwrap().pane_overlays.open_panes(),
            vec![OverlayPane::Left]
        );
    }

    #[test]
    fn closing_records_the_status_and_keeps_it_until_the_next_open() {
        let (mut store, id) = store_with_session();
        store
            .open_pane_overlay(id, OverlayPane::Left, program("exit 4"))
            .unwrap();
        let file = store
            .session(id)
            .unwrap()
            .pane_overlay(OverlayPane::Left)
            .unwrap()
            .code_file
            .clone()
            .unwrap();
        std::fs::write(&file, "4\n").unwrap();
        assert!(store.close_pane_overlay(id, OverlayPane::Left));
        assert!(!file.exists(), "the status file is consumed");
        assert_eq!(store.session(id).unwrap().pane_overlays.left.exit_code, Some(4));
        assert!(!store.close_pane_overlay(id, OverlayPane::Left), "nothing open");
        store.open_pane_overlay(id, OverlayPane::Left, program("cat")).unwrap();
        assert_eq!(
            store.session(id).unwrap().pane_overlays.left.exit_code,
            None,
            "a new open clears it"
        );
    }

    #[test]
    fn generations_are_unique_and_follow_swaps_and_promotions() {
        let (mut store, id) = store_with_session();
        split(&mut store, id);
        store.open_pane_overlay(id, OverlayPane::Left, program("a")).unwrap();
        store.open_pane_overlay(id, OverlayPane::Right, program("b")).unwrap();
        let session = store.session_mut(id).unwrap();
        let (left, right) = (
            session.pane_overlay(OverlayPane::Left).unwrap().generation,
            session.pane_overlay(OverlayPane::Right).unwrap().generation,
        );
        assert_ne!(left, right);
        session.pane_overlays.swap();
        assert_eq!(session.pane_overlays.pane_of(left), Some(OverlayPane::Right));
        session.pane_overlays.teardown(OverlayPane::Left);
        session.pane_overlays.promote();
        assert_eq!(session.pane_overlays.pane_of(left), Some(OverlayPane::Left));
        assert_eq!(session.pane_overlays.pane_of(right), None);
        assert!(session.pane_overlays.right.overlay.is_none());
    }

    #[test]
    fn an_unrealized_overlay_off_its_pane_is_dropped_and_a_realized_one_kept() {
        let (mut store, id) = store_with_session();
        split(&mut store, id);
        store.open_pane_overlay(id, OverlayPane::Left, program("a")).unwrap();
        store.open_pane_overlay(id, OverlayPane::Right, program("b")).unwrap();
        let session = store.session_mut(id).unwrap();
        session
            .pane_overlays
            .right
            .surface
            .replace(Box::new(FakeSurface::new("")));
        session
            .pane_overlays
            .left
            .surface
            .replace(Box::new(FakeSurface::new("").with_realized(false)));
        // hide the split behind the focused right pane: the left one is no longer laid out
        session.split.shown = false;
        session.split_surface.replace(Box::new(FakeSurface::new("R")));
        assert!(
            !session.drop_unrealized_pane_overlays(|pane| pane == OverlayPane::Left),
            "a zoom hosts it"
        );
        assert!(session.drop_unrealized_pane_overlays(|_| false));
        assert!(session.pane_overlay(OverlayPane::Left).is_none());
        session.split.focused = false;
        assert!(
            !session.drop_unrealized_pane_overlays(|_| false),
            "the realized one stays"
        );
        assert!(session.pane_overlay(OverlayPane::Right).is_some());
    }
}
