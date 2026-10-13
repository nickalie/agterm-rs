//! The split and scratch panes' mutations (upstream's `AppStore+Panes.swift`): showing, hiding, resizing, swapping
//! and closing the split, promoting a split survivor when the primary pane's shell exits, and the scratch terminal.
//!
//! A pane's status tag and search owner are roles here, not surface identities as upstream keeps them, so the paths
//! that move a terminal to another role (swap, promotion) re-tag them along with it.

use agterm_proto::EventKind;
use uuid::Uuid;

use super::outbox::EventPayload;
use super::overlay::OverlayPane;
use super::pane::{PaneLaunch, PaneRestore, SplitAxis, SplitLayout, StatusPane, clamp_split_ratio};
use super::session::{AgentIndicator, AgentStatus, Session};
use super::store::AppStore;
use super::workspace::Workspace;

/// Why [`AppStore::swap_panes`] could not exchange a session's panes (upstream's `SwapRefusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapRefusal {
    /// The session is gone.
    NoSession,
    /// The session has no split pane.
    NoSplit,
    /// A pane's terminal has not been created yet.
    SlotNotRealized,
}

impl AppStore {
    /// Toggles the one-level split. With no axis this hides a shown split and shows a hidden or absent one, keeping
    /// the axis; with an axis, the same shown axis hides, another shown axis transposes, and a hidden or absent split
    /// shows in that arrangement (upstream's `toggleSplit`).
    pub fn toggle_split(&mut self, id: Uuid, axis: Option<SplitAxis>) {
        let Some(session) = self.session_mut(id) else { return };
        let show = match axis {
            Some(axis) => {
                let show = !session.split.shown || session.split.axis != axis;
                if show {
                    session.split.axis = axis;
                }
                show
            }
            None => !session.split.shown,
        };
        self.apply_split_visibility(id, show);
    }

    /// Shows or hides the split idempotently, taking `axis` when showing; hiding keeps the axis (upstream's
    /// `setSplitVisibility`).
    pub fn set_split_visibility(&mut self, id: Uuid, shown: bool, axis: Option<SplitAxis>) {
        let Some(session) = self.session_mut(id) else { return };
        if shown && let Some(axis) = axis {
            session.split.axis = axis;
        }
        self.apply_split_visibility(id, shown);
    }

    /// A new split mints the right pane's identity and focuses it; re-showing a hidden one keeps the pane focused
    /// before the hide, and hiding keeps the split's existence and focus, so the focused pane shows maximized.
    fn apply_split_visibility(&mut self, id: Uuid, shown: bool) {
        let Some(session) = self.session_mut(id) else { return };
        let was_shown = session.split.shown;
        session.split.shown = shown;
        if shown && !session.split.exists {
            session.split.exists = true;
            session.split_pane_identity = Some(Uuid::new_v4());
            session.split.focused = true;
        }
        // hiding the split un-renders a pane, so an overlay opened on it that never came up would sit forever: upstream
        // drops it here; the store cannot see the zoom that may host it, so `Core::commit` drops it, sparing that zoom
        if was_shown != shown {
            self.emit_pane_visibility(EventKind::PaneSplit, id, shown);
        }
        self.save();
    }

    /// Sets the split divider's primary-pane fraction, clamped, and saves; the applied fraction, `None` for an
    /// unknown session (upstream's `applySplitRatio`).
    pub fn apply_split_ratio(&mut self, ratio: f64, id: Uuid) -> Option<f64> {
        let session = self.session_mut(id)?;
        let applied = clamp_split_ratio(ratio);
        session.split.ratio = Some(applied);
        self.save();
        Some(applied)
    }

    /// Moves focus between the panes of a split, hidden included: a hidden split then shows the chosen pane maximized
    /// (upstream's `setSplitFocus`). False without a split.
    pub fn set_split_focus(&mut self, id: Uuid, to_split: bool) -> bool {
        match self.session_mut(id) {
            Some(session) if session.split.exists => {
                session.split.focused = to_split;
                true
            }
            _ => false,
        }
    }

    /// A pane's terminal took keyboard focus because the user moved it there (a click): with both panes laid out the
    /// focused pane follows it. A report about a pane the layout does not show is stale (focus the frontend moved
    /// while panes came and went) and changes nothing, so a hidden split keeps its chosen pane. Whether it changed.
    pub fn note_pane_focus(&mut self, id: Uuid, pane: StatusPane) -> bool {
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        let to_split = match pane {
            StatusPane::Left => false,
            StatusPane::Right => true,
            StatusPane::Scratch => return false,
        };
        if !session.split.exists || !session.split.shown || session.scratch_active || session.split.focused == to_split
        {
            return false;
        }
        session.split.focused = to_split;
        true
    }

    /// Exchanges the two panes' terminals and everything each pane owns; the layout (axis, ratio, shown) stays and
    /// focus follows the terminal (upstream's `swapPanes`).
    ///
    /// # Errors
    ///
    /// The refusal, with nothing changed.
    pub fn swap_panes(&mut self, id: Uuid) -> Result<(), SwapRefusal> {
        let session = self.session_mut(id).ok_or(SwapRefusal::NoSession)?;
        if !session.split.exists {
            return Err(SwapRefusal::NoSplit);
        }
        if !session.surface.is_present() || !session.split_surface.is_present() {
            return Err(SwapRefusal::SlotNotRealized);
        }
        swap_pane_fields(session);
        self.save();
        Ok(())
    }

    /// Closes the split pane: hides it and tears its terminal down, dropping everything it owned, so a later split
    /// starts a fresh shell at an even ratio (upstream's `closeSplit`).
    pub fn close_split(&mut self, id: Uuid) {
        let split_identity = self.session(id).and_then(|session| session.split_pane_identity);
        self.close_hud_owned_by(id, split_identity);
        self.cancel_ask_owned_by(id, split_identity);
        let Some(session) = self.session_mut(id) else { return };
        let was_shown = session.split.shown;
        session.split = SplitLayout::default();
        session.split_surface.teardown();
        // the right pane is gone: its overlay has nothing left to cover and nobody left to read its status
        session.pane_overlays.teardown(OverlayPane::Right);
        session.split_cwd = None;
        session.split_title = None;
        session.initial_split_cwd = None;
        // the override's text file is named by the identity dropped next
        let removal = session.pane_text_removal(StatusPane::Right);
        session.pane_backgrounds.right = None;
        session.split_pane_identity = None;
        session.split_restore = PaneRestore::default();
        session.split_launch = PaneLaunch::default();
        session.search.clear();
        self.outbox.removed_watermarks.extend(removal);
        self.clear_indicator_owned_by(StatusPane::Right, id);
        if was_shown {
            self.emit_pane_visibility(EventKind::PaneSplit, id, false);
        }
        self.save();
    }

    /// The primary pane's shell exited: a split survivor moves into the primary slot with its cwd, title, creation
    /// command and restore state, and the session lives on as one pane; without one the session closes (upstream's
    /// `closePrimaryPane`).
    pub fn close_primary_pane(&mut self, id: Uuid) {
        let has_survivor = self
            .session(id)
            .is_some_and(|session| session.split_surface.is_present());
        if !has_survivor {
            self.close_session(id);
            return;
        }
        let primary_identity = self.session(id).map(|session| session.pane_identity);
        self.close_hud_owned_by(id, primary_identity);
        self.cancel_ask_owned_by(id, primary_identity);
        let Some(session) = self.session_mut(id) else { return };
        let was_shown = session.split.shown;
        // the exiting primary's override goes with it; the survivor's moves left, its file named by the same identity
        let removal = session.pane_text_removal(StatusPane::Left);
        promote_split_survivor(session);
        self.outbox.removed_watermarks.extend(removal);
        self.retag_promoted_indicator(id);
        if was_shown {
            self.emit_pane_visibility(EventKind::PaneSplit, id, false);
        }
        self.save();
    }

    /// Closes session `id`'s HUD when it is scoped to the pane `identity` names, before that identity goes or moves
    /// (upstream's `hudPaneIdentity == ... { closeHud }`): an unresolved identity would read as a session-wide HUD.
    fn close_hud_owned_by(&mut self, id: Uuid, identity: Option<Uuid>) {
        let owned = identity.is_some_and(|identity| {
            self.session(id)
                .and_then(|session| session.overlay.hud.as_ref())
                .is_some_and(|hud| hud.pane_identity == Some(identity))
        });
        if owned {
            self.close_hud(id);
        }
    }

    /// The split pane's shell exited: a live two-pane split collapses to the primary; otherwise this was the
    /// session's last pane (a split pane whose primary slot is empty) and the session closes (upstream's
    /// `closeSplitPane`).
    pub fn close_split_pane(&mut self, id: Uuid) {
        let two_panes = self
            .session(id)
            .is_some_and(|session| session.surface.is_present() && session.split_surface.is_present());
        if two_panes {
            self.close_split(id);
        } else {
            self.close_session(id);
        }
    }

    /// Shows or hides the scratch terminal; its terminal stays alive while hidden. `emit` false is for a respawn whose
    /// shown state ends where it started (upstream's `toggleScratch`). Not persisted.
    pub fn toggle_scratch(&mut self, id: Uuid, emit: bool) {
        let Some(session) = self.session_mut(id) else { return };
        session.scratch_active = !session.scratch_active;
        let shown = session.scratch_active;
        if emit {
            self.emit_pane_visibility(EventKind::PaneScratch, id, shown);
        }
    }

    /// Closes the scratch terminal: hides it and tears its terminal down, so the next show starts fresh (upstream's
    /// `closeScratch`). False without a scratch terminal.
    pub fn close_scratch(&mut self, id: Uuid, emit: bool) -> bool {
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        if !session.scratch_surface.is_present() {
            return false;
        }
        let was_shown = session.scratch_active;
        session.scratch_active = false;
        if session.search.owner == Some(StatusPane::Scratch) {
            session.search.clear();
        }
        let removal = session.pane_text_removal(StatusPane::Scratch);
        session.pane_backgrounds.scratch = None;
        session.scratch_surface.teardown();
        self.outbox.removed_watermarks.extend(removal);
        if was_shown && emit {
            self.emit_pane_visibility(EventKind::PaneScratch, id, false);
        }
        self.clear_indicator_owned_by(StatusPane::Scratch, id);
        true
    }

    /// `pane.split`/`pane.scratch` with `shown` or `hidden` (upstream's `emitPaneVisibility`).
    fn emit_pane_visibility(&mut self, kind: EventKind, id: Uuid, shown: bool) {
        let workspace = self.workspace_for_session(id).map(Workspace::id);
        let name = self.session(id).map(Session::display_name);
        let payload = EventPayload {
            name,
            status: Some(if shown { "shown" } else { "hidden" }.to_owned()),
            ..EventPayload::default()
        };
        self.emit(kind, workspace, Some(id), payload);
    }

    /// Clears the status when the departing pane `owner` owned it (an untagged status is the main pane's), so no glyph
    /// outlives every pane that could keystroke-clear it (upstream's `clearIndicatorOwnedByPane`).
    fn clear_indicator_owned_by(&mut self, owner: StatusPane, id: Uuid) {
        let departs = self.session(id).is_some_and(|session| {
            let indicator = &session.agent_indicator;
            indicator.status != AgentStatus::Idle && indicator.status_pane.unwrap_or(StatusPane::Left) == owner
        });
        if departs {
            self.set_agent_indicator(AgentIndicator::default(), id);
        }
    }

    /// After a promotion the exited primary's status (left or untagged) dies with it and the survivor's right tag
    /// follows it as left; a scratch tag stays.
    fn retag_promoted_indicator(&mut self, id: Uuid) {
        let Some(indicator) = self.session(id).map(|session| session.agent_indicator.clone()) else {
            return;
        };
        if indicator.status == AgentStatus::Idle {
            return;
        }
        match indicator.status_pane.unwrap_or(StatusPane::Left) {
            StatusPane::Left => self.set_agent_indicator(AgentIndicator::default(), id),
            StatusPane::Right => {
                let promoted = AgentIndicator {
                    status_pane: Some(StatusPane::Left),
                    ..indicator
                };
                self.set_agent_indicator(promoted, id);
            }
            StatusPane::Scratch => {}
        }
    }
}

/// The field exchange of [`AppStore::swap_panes`], both slots known present.
fn swap_pane_fields(session: &mut Session) {
    let primary_cwd = session.effective_cwd().to_owned();
    let split_cwd = session.cwd_for(StatusPane::Right).to_owned();
    std::mem::swap(&mut session.surface, &mut session.split_surface);
    if let Some(split_identity) = session.split_pane_identity {
        session.split_pane_identity = Some(session.pane_identity);
        session.pane_identity = split_identity;
    }
    session.current_cwd = Some(split_cwd);
    session.split_cwd = Some(primary_cwd.clone());
    session.initial_split_cwd = Some(primary_cwd);
    std::mem::swap(&mut session.osc_title, &mut session.split_title);
    std::mem::swap(&mut session.restore, &mut session.split_restore);
    std::mem::swap(&mut session.launch, &mut session.split_launch);
    let backgrounds = &mut session.pane_backgrounds;
    std::mem::swap(&mut backgrounds.left, &mut backgrounds.right);
    session.search.owner = session.search.owner.map(other_pane);
    let indicator = &mut session.agent_indicator;
    indicator.status_pane = if indicator.status == AgentStatus::Idle {
        None
    } else {
        match indicator.status_pane {
            None | Some(StatusPane::Left) => Some(StatusPane::Right),
            Some(StatusPane::Right) => Some(StatusPane::Left),
            Some(StatusPane::Scratch) => Some(StatusPane::Scratch),
        }
    };
    session.split.focused = !session.split.focused;
    session.pane_overlays.swap();
}

/// The other pane of a split; the scratch stays.
fn other_pane(pane: StatusPane) -> StatusPane {
    match pane {
        StatusPane::Left => StatusPane::Right,
        StatusPane::Right => StatusPane::Left,
        StatusPane::Scratch => StatusPane::Scratch,
    }
}

/// The model side of [`AppStore::close_primary_pane`] with a survivor in the split slot.
fn promote_split_survivor(session: &mut Session) {
    session.surface.teardown();
    // the pane overlays follow their panes: the exiting primary's dies with it, the survivor's moves left with its
    // exit status
    session.pane_overlays.teardown(OverlayPane::Left);
    session.pane_overlays.promote();
    if let Some(mut survivor) = session.split_surface.take() {
        survivor.promote_to_primary();
        session.surface.replace(survivor);
    }
    let backgrounds = &mut session.pane_backgrounds;
    backgrounds.left = backgrounds.right.take();
    session.pane_identity = session.split_pane_identity.take().unwrap_or_else(Uuid::new_v4);
    session.split = SplitLayout::default();
    session.launch = std::mem::take(&mut session.split_launch);
    session.current_cwd = session
        .split_cwd
        .take()
        .or_else(|| session.initial_split_cwd.take())
        .or_else(|| session.current_cwd.take());
    session.initial_split_cwd = None;
    session.osc_title = session.split_title.take();
    // as upstream: the pin, the capture and the armed override follow the survivor. The capture still armed for
    // this launch follows it too (upstream leaves it in the split slot): a survivor whose paced grant arrives after
    // the promotion still replays it, and the next split the session opens does not inherit a gone pane's program
    let split = &mut session.split_restore;
    session.restore.foreground = split.foreground.take();
    session.restore.command = split.command.take();
    session.restore.pending_command = split.pending_command.take();
    session.restore.pending_foreground = split.pending_foreground.take();
    // a search the survivor owned follows it into the left role; the exited primary's ends
    match session.search.owner {
        Some(StatusPane::Right) => session.search.owner = Some(StatusPane::Left),
        Some(StatusPane::Scratch) => {}
        Some(StatusPane::Left) | None => session.search.clear(),
    }
}
