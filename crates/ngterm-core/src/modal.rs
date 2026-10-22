//! The window modal slot and the retained outcomes of asks and picks (upstream's `PickController`, `PickRegistry`
//! and `AskRegistry` in `Pick.swift` and `Ask.swift`).
//!
//! Each open window has one modal slot, held by a native picker or a GUI ask, never both. A terminal ask waits on its
//! session instead ([`crate::model::SessionAsk`]) and only its id is indexed here. Outcomes stay readable after their
//! dialog ended, so a polling client that arrives late still reads its answer: a pick's per window and, once the
//! window is gone, app-wide; an ask's app-wide.

use std::collections::{HashMap, VecDeque};

use ngterm_proto::{ControlAskOutcome, ControlAskResult, ControlPickOutcome, ControlPickResult, PickItem};
use uuid::Uuid;

use crate::model::{EndedAsk, PendingAsk};

/// The most items one picker takes (upstream's `ControlPickItem.maxItems`).
pub const MAX_PICK_ITEMS: usize = 1_000;

/// Terminal pick results kept per window: a blocking client polls every 500 ms, so its answer survives any plausible
/// number of pickers opened in that window before the poll lands.
const WINDOW_RETAINED_PICKS: usize = 8;
/// Pick results kept after their window went away, across the app.
const APP_RETAINED_PICKS: usize = 32;
/// Ask outcomes kept after their dialog ended, across the app.
const RETAINED_ASKS: usize = 32;

/// `pick pending` (upstream's `pendingModalError` with a pick in the slot).
pub const PICK_PENDING: &str = "pick pending";
/// `ask pending` (the same with a GUI ask in the slot).
pub const ASK_PENDING: &str = "ask pending";

/// One picker a window presents (upstream's `PendingPick`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPick {
    /// The globally unique picker id (an upper-case UUID).
    pub id: String,
    pub items: Vec<PickItem>,
    pub prompt: Option<String>,
    /// The text the query field opens with; a non-empty one filters at once.
    pub query: Option<String>,
    /// Whether typed text that matches no row is an answer.
    pub allow_custom: bool,
    /// The item id the picker opens highlighted on, validated against `items`.
    pub selection: Option<String>,
}

impl PendingPick {
    /// The outcome of picking the item at `index` in the caller's order; `None` past the last item.
    #[must_use]
    pub fn picked(&self, index: usize) -> Option<ControlPickResult> {
        let item = self.items.get(index)?;
        Some(ControlPickResult {
            id: Some(item.id.clone()),
            label: Some(item.label.clone()),
            index: i64::try_from(index).ok(),
            ..ControlPickResult::outcome(ControlPickOutcome::Picked)
        })
    }

    /// The outcome of answering with typed text.
    #[must_use]
    pub fn custom(query: &str) -> ControlPickResult {
        ControlPickResult {
            query: Some(query.to_owned()),
            ..ControlPickResult::outcome(ControlPickOutcome::Custom)
        }
    }
}

/// A finished pick and when it was answered, app-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPick {
    id: String,
    result: ControlPickResult,
    sequence: u64,
}

/// One window's modal slot and its recent pick results (upstream's `PickController`).
#[derive(Debug, Default)]
pub struct PickController {
    pending: Option<PendingPick>,
    pending_ask: Option<PendingAsk>,
    /// Terminal results, oldest first.
    recent: VecDeque<ResolvedPick>,
}

impl PickController {
    /// The picker the slot holds.
    #[must_use]
    pub fn pending(&self) -> Option<&PendingPick> {
        self.pending.as_ref()
    }

    /// The GUI ask the slot holds.
    #[must_use]
    pub fn pending_ask(&self) -> Option<&PendingAsk> {
        self.pending_ask.as_ref()
    }

    /// Whether a picker or a GUI ask holds the slot.
    #[must_use]
    pub fn modal_pending(&self) -> bool {
        self.pending.is_some() || self.pending_ask.is_some()
    }

    /// What blocks another modal action while the slot is held (upstream's `pendingModalError`).
    #[must_use]
    pub fn pending_modal_error(&self) -> Option<&'static str> {
        if self.pending_ask.is_some() {
            Some(ASK_PENDING)
        } else if self.pending.is_some() {
            Some(PICK_PENDING)
        } else {
            None
        }
    }

    /// The pending or retained result of picker `id`.
    #[must_use]
    pub fn result(&self, id: &str) -> Option<ControlPickResult> {
        if self.pending.as_ref().is_some_and(|pick| pick.id == id) {
            return Some(ControlPickResult::outcome(ControlPickOutcome::Pending));
        }
        self.recent
            .iter()
            .rev()
            .find(|resolved| resolved.id == id)
            .map(|resolved| resolved.result.clone())
    }
}

/// Who holds a live ask (upstream's `AskRegistry.Owner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskOwner {
    /// A GUI ask in the window's modal slot.
    Window(Uuid),
    /// A terminal ask on a session of the window.
    Session { session: Uuid, window: Uuid },
}

impl AskOwner {
    /// The window the ask belongs to, which scopes its result.
    #[must_use]
    pub fn window(self) -> Uuid {
        match self {
            Self::Window(window) | Self::Session { window, .. } => window,
        }
    }
}

/// Every window's modal slot, the live asks' owners and the retained outcomes.
#[derive(Debug, Default)]
pub struct Modals {
    controllers: HashMap<Uuid, PickController>,
    /// Pick results whose window went away, oldest answer first.
    retained_picks: Vec<(Uuid, ResolvedPick)>,
    /// Orders pick answers across windows.
    sequence: u64,
    ask_owners: HashMap<String, AskOwner>,
    /// Ended asks: id, outcome and owning window, oldest first.
    finished_asks: VecDeque<(String, ControlAskResult, Uuid)>,
    /// The windows whose open modal currently pauses their auto-follow.
    pub(crate) suppressing: Vec<Uuid>,
}

impl Modals {
    /// Window `window`'s slot; `None` until the window registered one.
    #[must_use]
    pub fn controller(&self, window: Uuid) -> Option<&PickController> {
        self.controllers.get(&window)
    }

    /// Window `window`'s slot, registered on first use.
    pub(crate) fn controller_mut(&mut self, window: Uuid) -> &mut PickController {
        self.controllers.entry(window).or_default()
    }

    /// The windows with a registered slot.
    pub(crate) fn windows(&self) -> Vec<Uuid> {
        self.controllers.keys().copied().collect()
    }

    /// What blocks a modal action in window `window`, if anything.
    #[must_use]
    pub fn pending_modal_error(&self, window: Uuid) -> Option<&'static str> {
        self.controllers
            .get(&window)
            .and_then(PickController::pending_modal_error)
    }

    // MARK: - picks

    /// Puts `pick` in window `window`'s slot; refused with what holds the slot (upstream's `open`).
    pub(crate) fn open_pick(&mut self, window: Uuid, pick: PendingPick) -> Result<(), &'static str> {
        let controller = self.controller_mut(window);
        if let Some(held) = held_by(controller) {
            return Err(held);
        }
        controller.pending = Some(pick);
        Ok(())
    }

    /// Ends window `window`'s pending picker with `outcome`, keeping the result readable (upstream's `resolve`).
    pub(crate) fn resolve_pick(&mut self, window: Uuid, outcome: ControlPickResult) {
        self.sequence += 1;
        let sequence = self.sequence;
        let Some(controller) = self.controllers.get_mut(&window) else {
            return;
        };
        let Some(pending) = controller.pending.take() else {
            return;
        };
        controller.recent.retain(|resolved| resolved.id != pending.id);
        controller.recent.push_back(ResolvedPick {
            id: pending.id,
            result: outcome,
            sequence,
        });
        while controller.recent.len() > WINDOW_RETAINED_PICKS {
            controller.recent.pop_front();
        }
    }

    /// The live picker `id` and its window, looked up by its globally unique id (upstream's `livePick`).
    #[must_use]
    pub(crate) fn live_pick(&self, id: &str) -> Option<(Uuid, ControlPickResult)> {
        self.controllers
            .iter()
            .find_map(|(window, controller)| controller.result(id).map(|result| (*window, result)))
    }

    /// A result kept after its window went away (upstream's `retainedResult`).
    #[must_use]
    pub(crate) fn retained_pick(&self, id: &str) -> Option<(Uuid, ControlPickResult)> {
        self.retained_picks
            .iter()
            .rev()
            .find(|(_, resolved)| resolved.id == id)
            .map(|(window, resolved)| (*window, resolved.result.clone()))
    }

    /// Drops window `window`'s slot: its pending picker and GUI ask end cancelled and its results stay readable
    /// app-wide (upstream's `unregister`).
    pub(crate) fn unregister(&mut self, window: Uuid) {
        if !self.controllers.contains_key(&window) {
            return;
        }
        self.resolve_pick(window, ControlPickResult::outcome(ControlPickOutcome::Cancelled));
        self.resolve_window_ask(window, ControlAskResult::outcome(ControlAskOutcome::Cancelled));
        let Some(controller) = self.controllers.remove(&window) else {
            return;
        };
        self.retained_picks
            .extend(controller.recent.into_iter().map(|resolved| (window, resolved)));
        if self.retained_picks.len() > APP_RETAINED_PICKS {
            // by when each pick was answered: a window closing later arrives with a whole batch
            self.retained_picks.sort_by_key(|(_, resolved)| resolved.sequence);
            let excess = self.retained_picks.len() - APP_RETAINED_PICKS;
            self.retained_picks.drain(..excess);
        }
    }

    // MARK: - asks

    /// Puts GUI ask `ask` in window `window`'s slot and indexes it; refused with what holds the slot.
    pub(crate) fn open_window_ask(&mut self, window: Uuid, ask: PendingAsk) -> Result<(), &'static str> {
        let id = ask.id.clone();
        let controller = self.controller_mut(window);
        if let Some(held) = held_by(controller) {
            return Err(held);
        }
        controller.pending_ask = Some(ask);
        self.register_ask(id, AskOwner::Window(window));
        Ok(())
    }

    /// Ends window `window`'s GUI ask with `outcome`, retaining it before the slot frees (upstream's `resolveAsk`).
    pub(crate) fn resolve_window_ask(&mut self, window: Uuid, outcome: ControlAskResult) {
        if outcome.result == ControlAskOutcome::Pending {
            return;
        }
        let Some(ask) = self
            .controllers
            .get_mut(&window)
            .and_then(|controller| controller.pending_ask.take())
        else {
            return;
        };
        if self.ask_owners.get(&ask.id) == Some(&AskOwner::Window(window)) {
            self.retain_ask(&ask.id, outcome, window);
        }
    }

    /// Indexes an opened ask (upstream's `register`); an id already live or retained is refused.
    pub(crate) fn register_ask(&mut self, id: String, owner: AskOwner) -> bool {
        if self.ask_owners.contains_key(&id) || self.finished_asks.iter().any(|(finished, ..)| *finished == id) {
            return false;
        }
        self.ask_owners.insert(id, owner);
        true
    }

    /// Moves a live ask to another owner (upstream's `reassign`), as a taken-back GUI ask moves from its session into
    /// its window's slot.
    pub(crate) fn reassign_ask(&mut self, id: &str, owner: AskOwner) {
        if let Some(current) = self.ask_owners.get_mut(id) {
            *current = owner;
        }
    }

    /// The live owner of ask `id`.
    #[must_use]
    pub fn ask_owner(&self, id: &str) -> Option<AskOwner> {
        self.ask_owners.get(id).copied()
    }

    /// Retains the outcomes of session asks that ended (upstream's `Session.resolveAsk` retaining through the
    /// registry); an ask its registered session does not own is ignored.
    pub(crate) fn retain_ended(&mut self, ended: Vec<EndedAsk>) {
        for ask in ended {
            if let Some(AskOwner::Session { session, window }) = self.ask_owner(&ask.id)
                && session == ask.session
            {
                self.retain_ask(&ask.id, ask.result, window);
            }
        }
    }

    /// The first terminal outcome wins (upstream's `retain`).
    fn retain_ask(&mut self, id: &str, result: ControlAskResult, window: Uuid) {
        if result.result == ControlAskOutcome::Pending || self.ask_owner(id).map(AskOwner::window) != Some(window) {
            return;
        }
        self.ask_owners.remove(id);
        self.finished_asks.push_back((id.to_owned(), result, window));
        while self.finished_asks.len() > RETAINED_ASKS {
            self.finished_asks.pop_front();
        }
    }

    /// The retained outcome of ask `id` and its window; a live ask reads through its owner instead.
    #[must_use]
    pub(crate) fn finished_ask(&self, id: &str) -> Option<(ControlAskResult, Uuid)> {
        self.finished_asks
            .iter()
            .find(|(finished, ..)| finished == id)
            .map(|(_, result, window)| (result.clone(), *window))
    }

    /// The GUI ask window `window`'s slot holds.
    #[must_use]
    pub fn window_ask(&self, window: Uuid) -> Option<&PendingAsk> {
        self.controllers.get(&window).and_then(PickController::pending_ask)
    }
}

fn held_by(controller: &PickController) -> Option<&'static str> {
    if controller.pending_ask.is_some() {
        Some("ask already pending")
    } else if controller.pending.is_some() {
        Some("pick already pending")
    } else {
        None
    }
}

// MARK: - the core's side

impl crate::Core {
    /// Window `window`'s modal slot, for the frontend to draw its picker or GUI ask.
    #[must_use]
    pub fn modal_slot(&self, window: Uuid) -> Option<&PickController> {
        self.modals.controller(window)
    }

    /// Whether a picker or a GUI ask holds window `window`'s slot: the frontend's shortcuts, menus and terminal focus
    /// stand down while it does (upstream's `pickActive`).
    #[must_use]
    pub fn modal_pending(&self, window: Uuid) -> bool {
        self.modals
            .controller(window)
            .is_some_and(PickController::modal_pending)
    }

    /// The user answered window `window`'s picker `id` (a row, typed text, or a dismissal). A stale id is ignored.
    /// Commit afterwards.
    pub fn resolve_pick(&mut self, window: Uuid, id: &str, outcome: ControlPickResult) {
        let current = self
            .modals
            .controller(window)
            .and_then(PickController::pending)
            .is_some_and(|pick| pick.id == id);
        if current {
            self.modals.resolve_pick(window, outcome);
            self.sync_modals();
        }
    }

    /// The user answered window `window`'s GUI ask `id` (a button, or `escaped`). A stale id is ignored. Commit
    /// afterwards.
    pub fn answer_window_ask(&mut self, window: Uuid, id: &str, outcome: ControlAskResult) {
        if self.modals.window_ask(window).is_some_and(|ask| ask.id == id) {
            self.modals.resolve_window_ask(window, outcome);
            self.sync_modals();
        }
    }

    /// The user answered session `session`'s terminal ask `id` (a button, or `escaped`). A stale id is ignored.
    /// Commit afterwards.
    pub fn answer_session_ask(&mut self, session: Uuid, id: &str, outcome: ControlAskResult) {
        let Some(window) = self.library().window_holding_session(session) else {
            return;
        };
        if let Some(store) = self.library_mut().store_mut(window) {
            // one a viewer presents is answered there, never here
            let remote = store
                .session(session)
                .and_then(crate::model::Session::ask)
                .is_some_and(|pending| pending.remote_owner.is_some());
            if !remote {
                store.resolve_session_ask(session, id, outcome);
            }
        }
        self.sync_modals();
    }

    /// Brings the modal state in line with the model, as upstream's views do when they render: ended session asks
    /// are retained, a closed window's slot is dropped (its picker and ask cancelled), a GUI ask whose anchor left the
    /// screen is cancelled, and a window whose slot just filled hides the quick terminal and pauses its auto-follow
    /// until the slot frees.
    pub(crate) fn sync_modals(&mut self) {
        let ended = self.library_mut().take_ended_asks();
        self.dismiss_remote_asks(&ended);
        self.answer_replica_asks(&ended);
        self.modals.retain_ended(ended);
        for window in self.modals.windows() {
            if !self.library().is_open(window) {
                self.modals.unregister(window);
            }
        }
        self.cancel_stray_window_asks();
        self.sync_modal_suppression();
    }

    /// Cancels the GUI asks whose anchor is no longer on screen (upstream's `askAnchorIsValid`): the anchored session
    /// must be its window's selection, and an anchored pane still laid out.
    fn cancel_stray_window_asks(&mut self) {
        for window in self.modals.windows() {
            let Some(anchor) = self.modals.window_ask(window).and_then(|ask| ask.anchor.clone()) else {
                continue;
            };
            let store = self.library().store(window);
            let valid = store.is_some_and(|store| {
                store.selected_session_id() == Some(anchor.session)
                    && store
                        .session(anchor.session)
                        .is_some_and(|session| match anchor.pane_identity {
                            None => anchor.pane.is_none(),
                            Some(identity) => session
                                .pane_role_for_identity(identity)
                                .is_some_and(|pane| session.renders_overlay_pane(pane)),
                        })
            });
            if !valid {
                self.modals
                    .resolve_window_ask(window, ControlAskResult::outcome(ControlAskOutcome::Cancelled));
            }
        }
    }

    /// Pairs each window's auto-follow suppression with its modal slot being held, and hides the quick terminal when
    /// a slot fills: the floating panel would sit above the picker with the keyboard.
    fn sync_modal_suppression(&mut self) {
        let held: Vec<Uuid> = self
            .modals
            .windows()
            .into_iter()
            .filter(|window| self.modal_pending(*window))
            .collect();
        let released: Vec<Uuid> = self
            .modals
            .suppressing
            .iter()
            .copied()
            .filter(|window| !held.contains(window))
            .collect();
        for window in released {
            self.modals.suppressing.retain(|suppressed| *suppressed != window);
            self.resume_auto_follow(window);
        }
        for window in held {
            if !self.modals.suppressing.contains(&window) {
                self.modals.suppressing.push(window);
                self.suppress_auto_follow(window);
                self.quick.hide();
            }
        }
    }

    /// The quit path: every pending picker and ask, in every window, ends cancelled.
    pub(crate) fn cancel_all_modals(&mut self) {
        for window in self.library().open_ids() {
            if let Some(store) = self.library_mut().store_mut(window) {
                store.cancel_all_session_asks();
            }
        }
        for window in self.modals.windows() {
            self.modals
                .resolve_pick(window, ControlPickResult::outcome(ControlPickOutcome::Cancelled));
            self.modals
                .resolve_window_ask(window, ControlAskResult::outcome(ControlAskOutcome::Cancelled));
        }
        self.sync_modals();
    }
}

impl crate::Core {
    /// Whether the frontmost window's slot is held, which refuses the quick terminal's show (upstream's `canShow`).
    pub(crate) fn active_window_modal_pending(&self) -> bool {
        self.library()
            .active_window_id()
            .is_some_and(|window| self.modal_pending(window))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AskAlignment, AskStyle};

    fn pick(id: &str) -> PendingPick {
        PendingPick {
            id: id.to_owned(),
            items: Vec::new(),
            prompt: None,
            query: None,
            allow_custom: true,
            selection: None,
        }
    }

    fn ask(id: &str) -> PendingAsk {
        PendingAsk {
            id: id.to_owned(),
            title: "T".to_owned(),
            message: None,
            buttons: Vec::new(),
            default_id: None,
            destructive_id: None,
            style: AskStyle::Gui,
            align: AskAlignment::Right,
            width: None,
            anchor: None,
        }
    }

    fn outcome(result: ControlAskOutcome) -> ControlAskResult {
        ControlAskResult::outcome(result)
    }

    /// Upstream's `PickTests.retainedResultsStopAtTheLimitDroppingTheOldest`, app-wide: windows answer their picks
    /// interleaved and close in the reverse order, and the newest answers survive whichever window brought them.
    #[test]
    fn retained_results_keep_the_newest_answers_across_windows() {
        let mut modals = Modals::default();
        let windows: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        // round `round`'s pick of window `w` is answer number `round * 5 + w`
        for round in 0..WINDOW_RETAINED_PICKS {
            for (w, window) in windows.iter().enumerate() {
                modals.open_pick(*window, pick(&format!("pick-{round}-{w}"))).unwrap();
                modals.resolve_pick(*window, PendingPick::custom(&format!("{}", round * 5 + w)));
            }
        }
        for window in windows.iter().rev() {
            modals.unregister(*window);
        }
        assert_eq!(modals.retained_picks.len(), APP_RETAINED_PICKS);
        // 40 answers, 32 kept: the 8 oldest (round 0, and round 1 of windows 0..3) are gone
        for round in 0..WINDOW_RETAINED_PICKS {
            for (w, window) in windows.iter().enumerate() {
                let id = format!("pick-{round}-{w}");
                let answer = round * 5 + w;
                let expected = (answer >= 8).then(|| (*window, PendingPick::custom(&format!("{answer}"))));
                assert_eq!(modals.retained_pick(&id), expected, "{id}");
            }
        }
    }

    /// Upstream's `AskTests.registryEvictsByResolutionOrderAndNeverEvictsPendingAsks`.
    #[test]
    fn the_registry_evicts_by_resolution_order_and_never_a_pending_ask() {
        let mut modals = Modals::default();
        let windows: Vec<Uuid> = (0..RETAINED_ASKS + 3).map(|_| Uuid::new_v4()).collect();
        for (index, window) in windows.iter().enumerate() {
            modals.open_window_ask(*window, ask(&format!("ask-{index}"))).unwrap();
        }
        for (index, window) in windows.iter().enumerate().skip(1).rev() {
            modals.resolve_window_ask(*window, outcome(ControlAskOutcome::Cancelled));
            assert_eq!(modals.ask_owner(&format!("ask-{index}")), None, "ask-{index} ended");
        }
        assert_eq!(modals.ask_owner("ask-0"), Some(AskOwner::Window(windows[0])));
        assert_eq!(modals.window_ask(windows[0]).map(|ask| ask.id.as_str()), Some("ask-0"));
        assert_eq!(
            modals.finished_ask("ask-0"),
            None,
            "a pending ask reads through its owner"
        );
        for (index, window) in windows.iter().enumerate().take(RETAINED_ASKS + 1).skip(1) {
            assert_eq!(
                modals.finished_ask(&format!("ask-{index}")),
                Some((outcome(ControlAskOutcome::Cancelled), *window)),
                "ask-{index}"
            );
        }
        for index in [RETAINED_ASKS + 1, RETAINED_ASKS + 2] {
            assert_eq!(
                modals.finished_ask(&format!("ask-{index}")),
                None,
                "ask-{index} resolved first"
            );
        }
    }

    /// Upstream's `AskTests.registryRejectsDuplicateIDsWrongWindowsAndNonterminalRetention`.
    #[test]
    fn the_registry_rejects_duplicates_wrong_windows_and_nonterminal_outcomes() {
        let mut modals = Modals::default();
        let window = Uuid::new_v4();
        modals.open_window_ask(window, ask("original")).unwrap();
        assert!(!modals.register_ask("original".to_owned(), AskOwner::Window(Uuid::new_v4())));
        assert_eq!(modals.ask_owner("original"), Some(AskOwner::Window(window)));
        modals.retain_ask("original", outcome(ControlAskOutcome::Pending), window);
        modals.retain_ask("original", outcome(ControlAskOutcome::Cancelled), Uuid::new_v4());
        assert_eq!(
            modals.ask_owner("original"),
            Some(AskOwner::Window(window)),
            "still live"
        );
        assert_eq!(modals.finished_ask("original"), None);

        modals.retain_ask("original", outcome(ControlAskOutcome::Escaped), window);
        modals.retain_ask("original", outcome(ControlAskOutcome::Cancelled), window);
        assert!(!modals.register_ask("original".to_owned(), AskOwner::Window(window)));
        assert_eq!(
            modals.finished_ask("original"),
            Some((outcome(ControlAskOutcome::Escaped), window)),
            "the first terminal outcome wins"
        );
    }
}
