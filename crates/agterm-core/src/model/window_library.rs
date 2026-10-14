//! The app-wide window set (upstream's `WindowLibrary.swift`): ordered window metadata, the per-window stores of the
//! open windows, the frontmost window, and the side effects of all of them.
//!
//! A window is open iff its store is loaded. Loading a closed window's store from disk, the index file and the
//! launch bootstrap (migration and recovery) live in [`crate::persist::library`], which performs the saves this
//! library's effects ask for; the event ring comes with `events.read`.
//! Upstream's claim queue, which hands `SwiftUI`'s id-less `WindowGroup` windows their ids, has no counterpart: the GTK
//! frontend builds each window for a known id.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::ask::EndedAsk;
use super::naming::{default_window_name, is_auto_window_name, sanitized, trimmed_or_none};
use super::navigation::{WorkspaceNavigation, wrapped};
use super::outbox::{ModelEvent, RecentClosedChange, SaveRequest, WatermarkRemoval, next_emission};
use super::pending_close::PendingCloseTimer;
use super::recent_closed::RecentClosedList;
use super::session::Session;
use super::store::{AppStore, NewSession};
use crate::persist::{RecentClosedItem, RecentClosedKind};

/// One window's metadata (upstream's `WindowInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: Uuid,
    pub name: String,
}

impl WindowInfo {
    /// Whether the name is user-set; the title bar shows it only then, so "window N" stays hidden.
    #[must_use]
    pub fn has_custom_name(&self) -> bool {
        !is_auto_window_name(&self.name)
    }
}

/// One event of a window, as the library hands it to the event ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEvent {
    pub window: Uuid,
    pub event: ModelEvent,
}

/// What the library's mutations since the last drain ask of the world.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibraryEffects {
    /// Events in emission order, each tagged with its window.
    pub events: Vec<WindowEvent>,
    /// The snapshot write each open window asked for.
    pub window_saves: Vec<(Uuid, SaveRequest)>,
    /// Whether the window index changed and must be written.
    pub index_save: bool,
    /// Soft closes whose grace timer the caller arms (`AppStore::finalize_pending_close` when it fires).
    pub pending_closes: Vec<PendingCloseTimer>,
    /// The windows whose armed debounced write must be dropped unwritten, before `window_saves` apply.
    pub cancelled_saves: Vec<Uuid>,
    /// Open Recent changes for `recent-closed.json`, in the order they happened.
    pub recent_closed: Vec<RecentClosedChange>,
    /// Rendered text watermarks to delete from the state dir.
    pub removed_watermarks: Vec<WatermarkRemoval>,
    /// Pane identities removed for good, whose live-session daemons are ended (upstream's `paneFinalizer`).
    pub finalized_panes: Vec<Uuid>,
}

/// The app-global owner of the window set.
#[derive(Debug, Default)]
pub struct WindowLibrary {
    windows: Vec<WindowInfo>,
    stores: HashMap<Uuid, AppStore>,
    /// The id of the frontmost on-screen window. Outlives the window only when it was the last one closed, so the
    /// index records which window the user exited from; [`WindowLibrary::active_window_id`] resolves it to an open one.
    pub frontmost_window_id: Option<Uuid>,
    /// Set at quit, so the windows tearing down one by one leave the open set as it was for the next launch.
    pub is_terminating: bool,
    /// The home directory a new window's first session starts in.
    home: String,
    /// Library-level events (retired windows' outboxes, window renames and removals) waiting for the next drain,
    /// each with its place in the app-wide emission order.
    pending_events: Vec<(u64, WindowEvent)>,
    index_dirty: bool,
    /// Open Recent, newest first (upstream's `recentClosedItems`), shared with every open store.
    recent_closed: RecentClosedList,
    /// Open Recent changes the library itself made, and those of retired stores, with their emission places.
    pending_recent_closed: Vec<(u64, RecentClosedChange)>,
    /// Rendered text watermarks of removed windows' sessions and of retired stores, waiting for the next drain.
    pending_removed_watermarks: Vec<WatermarkRemoval>,
    /// Pane identities of removed windows' sessions and of retired stores, removed for good, for the next drain.
    pending_finalized_panes: Vec<Uuid>,
    /// Session asks that ended in stores no longer open, for [`WindowLibrary::take_ended_asks`].
    pending_ended_asks: Vec<EndedAsk>,
    /// Remote overlays closed in stores no longer open, for [`WindowLibrary::take_remote_overlay_closes`].
    pending_remote_overlay_closes: Vec<super::remote_overlay::RemoteOverlayClose>,
}

impl WindowLibrary {
    /// An empty library: no windows. A new window's first session starts in `home`.
    #[must_use]
    pub fn new(home: impl Into<String>) -> Self {
        Self {
            home: home.into(),
            ..Self::default()
        }
    }

    /// A library seeded with one fresh window, as a first launch with no state has.
    #[must_use]
    pub fn seeded(home: impl Into<String>) -> Self {
        let mut library = Self::new(home);
        library.new_window(None);
        library
    }

    // MARK: - Lookup

    /// The home directory: where a new window's first session starts, and a session's default cwd.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The window metadata in window order.
    #[must_use]
    pub fn windows(&self) -> &[WindowInfo] {
        &self.windows
    }

    /// The open window's store; `None` for a closed or unknown id.
    #[must_use]
    pub fn store(&self, id: Uuid) -> Option<&AppStore> {
        self.stores.get(&id)
    }

    pub fn store_mut(&mut self, id: Uuid) -> Option<&mut AppStore> {
        self.stores.get_mut(&id)
    }

    #[must_use]
    pub fn is_open(&self, id: Uuid) -> bool {
        self.stores.contains_key(&id)
    }

    /// The frontmost open window, else the first open one; `None` only when every window is closed.
    #[must_use]
    pub fn active_window_id(&self) -> Option<Uuid> {
        if let Some(frontmost) = self.frontmost_window_id
            && self.is_open(frontmost)
        {
            return Some(frontmost);
        }
        self.windows.iter().map(|info| info.id).find(|id| self.is_open(*id))
    }

    /// The store of [`WindowLibrary::active_window_id`]: what the action, control and settings seams act on.
    #[must_use]
    pub fn active_store(&self) -> Option<&AppStore> {
        self.active_window_id().and_then(|id| self.store(id))
    }

    pub fn active_store_mut(&mut self) -> Option<&mut AppStore> {
        let id = self.active_window_id()?;
        self.store_mut(id)
    }

    /// The open windows in window order.
    #[must_use]
    pub fn open_ids(&self) -> Vec<Uuid> {
        self.windows
            .iter()
            .map(|info| info.id)
            .filter(|id| self.is_open(*id))
            .collect()
    }

    /// Whether a window step has anywhere to go: closed windows are no candidates.
    #[must_use]
    pub fn can_step_windows(&self) -> bool {
        self.open_ids().len() > 1
    }

    /// The next or previous open window from the active one, wrapping; `None` below two open windows.
    #[must_use]
    pub fn navigate_window(&self, direction: WorkspaceNavigation) -> Option<Uuid> {
        let ids = self.open_ids();
        if ids.len() < 2 {
            return None;
        }
        let current = self.active_window_id()?;
        let index = ids.iter().position(|id| *id == current)?;
        Some(ids[wrapped(index, direction.step(), ids.len())])
    }

    /// The open window owning `session`; closed windows are not searched.
    #[must_use]
    pub fn window_id_for_session(&self, session: Uuid) -> Option<Uuid> {
        self.open_ids().into_iter().find(|id| {
            self.stores
                .get(id)
                .is_some_and(|store| store.session(session).is_some())
        })
    }

    /// The open store owning `session`: cross-window targeting.
    #[must_use]
    pub fn store_for_session(&self, session: Uuid) -> Option<&AppStore> {
        self.window_id_for_session(session).and_then(|id| self.store(id))
    }

    pub fn store_for_session_mut(&mut self, session: Uuid) -> Option<&mut AppStore> {
        let id = self.window_id_for_session(session)?;
        self.store_mut(id)
    }

    /// A window's name; empty for an unknown one (the `{AGT_WINDOW_NAME}` value).
    #[must_use]
    pub fn window_name(&self, id: Option<Uuid>) -> String {
        id.and_then(|id| self.info(id))
            .map(|info| info.name.clone())
            .unwrap_or_default()
    }

    /// A window's user-set name; `None` for an auto name or an unknown id.
    #[must_use]
    pub fn custom_window_name(&self, id: Uuid) -> Option<&str> {
        self.info(id)
            .filter(|info| info.has_custom_name())
            .map(|info| info.name.as_str())
    }

    fn info(&self, id: Uuid) -> Option<&WindowInfo> {
        self.windows.iter().find(|info| info.id == id)
    }

    /// The auto name of the next new window.
    #[must_use]
    pub fn default_window_name(&self) -> String {
        default_window_name(self.windows.len())
    }

    /// Every session of every open window, flattened.
    #[must_use]
    pub fn all_open_sessions(&self) -> Vec<&Session> {
        self.open_ids()
            .into_iter()
            .filter_map(|id| self.stores.get(&id))
            .flat_map(AppStore::sessions)
            .collect()
    }

    /// The unseen count across every session of every open window: the badge.
    #[must_use]
    pub fn total_unseen_count(&self) -> usize {
        self.all_open_sessions()
            .iter()
            .map(|session| session.unseen_count)
            .sum()
    }

    /// The open window count and their total session count, for the quit confirmation.
    #[must_use]
    pub fn open_counts(&self) -> (usize, usize) {
        let open = self.open_ids();
        let sessions = open
            .iter()
            .filter_map(|id| self.stores.get(id))
            .map(|store| store.sessions().count())
            .sum();
        (open.len(), sessions)
    }

    /// Whether a window may be removed: one is always kept.
    #[must_use]
    pub fn can_remove_window(&self) -> bool {
        self.windows.len() > 1
    }

    // MARK: - Mutation

    /// Creates a window seeded with "workspace 1" and one session at home, opens it and makes it frontmost, so the
    /// window-keyed seams target it at once. The name defaults to "window N". Returns the window id.
    pub fn new_window(&mut self, name: Option<&str>) -> Uuid {
        let name = name
            .and_then(|name| trimmed_or_none(&sanitized(name)))
            .unwrap_or_else(|| self.default_window_name());
        let id = Uuid::new_v4();
        let mut store = AppStore::new();
        store.recent_closed = self.recent_closed.share();
        let workspace = store.add_workspace("workspace 1");
        store.add_session(workspace, NewSession::at_cwd(self.home.clone()));
        self.windows.push(WindowInfo { id, name });
        self.stores.insert(id, store);
        self.frontmost_window_id = Some(id);
        self.index_dirty = true;
        id
    }

    /// Opens a window with an already built store (a load from disk). A no-op for an id with no metadata or an open
    /// window.
    pub fn open_window(&mut self, id: Uuid, mut store: AppStore) {
        if self.info(id).is_none() || self.is_open(id) {
            return;
        }
        store.recent_closed = self.recent_closed.share();
        self.stores.insert(id, store);
        self.index_dirty = true;
    }

    /// Adds window metadata without opening it (an index entry of a closed window).
    pub fn insert_window(&mut self, info: WindowInfo) {
        if self.info(info.id).is_none() {
            self.windows.push(info);
            self.index_dirty = true;
        }
    }

    /// Closes a window: tears its terminals down and drops its store, keeping its metadata. A no-op for an unknown
    /// or closed id, and while terminating. Closing the last open window pins it as frontmost, so the next launch
    /// reopens the window the user exited from.
    ///
    /// The store goes with any snapshot write it still has pending, so the caller writes the window's snapshot
    /// first (drain the effects and perform its save, or save it outright), as upstream's window `willClose` calls
    /// `store.save()` before `closeWindow`; the closed window's file is what its next open loads.
    pub fn close_window(&mut self, id: Uuid) {
        if self.is_terminating {
            return;
        }
        let Some(store) = self.stores.remove(&id) else { return };
        self.retire_store(id, store);
        if self.open_ids().is_empty() {
            self.frontmost_window_id = Some(id);
        } else if self.frontmost_window_id == Some(id) {
            self.frontmost_window_id = self.active_window_id();
        }
        self.index_dirty = true;
    }

    /// Renames a window; a blank or unchanged name is a no-op.
    pub fn rename_window(&mut self, id: Uuid, name: &str) {
        let Some(name) = trimmed_or_none(&sanitized(name)) else {
            return;
        };
        let Some(info) = self.windows.iter_mut().find(|info| info.id == id) else {
            return;
        };
        if info.name == name {
            return;
        }
        info.name = name;
        self.push_library_event(id, ModelEvent::bare(agterm_proto::EventKind::TreeChanged));
        self.index_dirty = true;
    }

    /// Removes a window for good: tears its terminals down, drops its store and its metadata. A no-op for the last
    /// window. Its pending snapshot write is dropped, so it cannot re-create the deleted file.
    pub fn remove_window(&mut self, id: Uuid) {
        if !self.can_remove_window() {
            return;
        }
        let Some(index) = self.windows.iter().position(|info| info.id == id) else {
            return;
        };
        if let Some(store) = self.stores.remove(&id) {
            // its sessions are destroyed for good, unlike a closed window's; a closed window's own sessions are swept
            // from its file by the persistence
            self.pending_removed_watermarks
                .extend(store.sessions().map(|session| WatermarkRemoval::Session(session.id())));
            self.pending_finalized_panes
                .extend(store.sessions().flat_map(Session::locally_managed_pane_identities));
            self.retire_store(id, store);
        }
        self.push_library_event(id, ModelEvent::bare(agterm_proto::EventKind::TreeChanged));
        self.windows.remove(index);
        if self.frontmost_window_id == Some(id) {
            self.frontmost_window_id = None;
        }
        self.index_dirty = true;
    }

    /// Retires a store leaving the open set and keeps its events and Open Recent changes for the next drain.
    fn retire_store(&mut self, id: Uuid, mut store: AppStore) {
        store.retire();
        self.pending_ended_asks.append(&mut store.take_ended_asks());
        self.pending_remote_overlay_closes
            .append(&mut store.take_remote_overlay_closes());
        let mut outbox = store.take_outbox();
        self.pending_recent_closed.extend(outbox.take_stamped_recent_closed());
        self.pending_removed_watermarks.append(&mut outbox.removed_watermarks);
        self.pending_finalized_panes.append(&mut outbox.finalized_panes);
        self.pending_events.extend(
            outbox
                .into_stamped_events()
                .map(|(emission, event)| (emission, WindowEvent { window: id, event })),
        );
    }

    /// Records an event the library itself emits for a window.
    fn push_library_event(&mut self, window: Uuid, event: ModelEvent) {
        self.pending_events
            .push((next_emission(), WindowEvent { window, event }));
    }

    /// Sets the frontmost window; recorded in the index.
    pub fn set_frontmost(&mut self, id: Option<Uuid>) {
        if self.frontmost_window_id != id {
            self.frontmost_window_id = id;
            self.index_dirty = true;
        }
    }

    /// Clears every session's font size in every open window (closed windows' files are rewritten by persistence).
    pub fn reset_session_font_sizes_all_windows(&mut self) {
        for store in self.stores.values_mut() {
            store.reset_session_font_sizes();
        }
    }

    /// Takes the session asks that ended since the last take, from every open store and every store that closed.
    pub fn take_ended_asks(&mut self) -> Vec<EndedAsk> {
        let mut ended = std::mem::take(&mut self.pending_ended_asks);
        for store in self.stores.values_mut() {
            ended.append(&mut store.take_ended_asks());
        }
        ended
    }

    /// Takes the remote overlays closed since the last take, from every open store and every store that closed.
    pub fn take_remote_overlay_closes(&mut self) -> Vec<super::remote_overlay::RemoteOverlayClose> {
        let mut closed = std::mem::take(&mut self.pending_remote_overlay_closes);
        for store in self.stores.values_mut() {
            closed.append(&mut store.take_remote_overlay_closes());
        }
        closed
    }

    /// Shows the frontmost open window's sidebar and hides every other one's (auto-hide inactive sidebars).
    pub fn apply_inactive_window_sidebar_hiding(&mut self) {
        let Some(active) = self.active_window_id() else { return };
        for id in self.open_ids() {
            if let Some(store) = self.stores.get_mut(&id) {
                store.set_sidebar_visible(id == active);
            }
        }
    }

    // MARK: - Effects

    /// Takes every side effect recorded since the last drain. The events of every store and of the library merge
    /// back into the order they were emitted in; the saves come in window order.
    pub fn drain_effects(&mut self) -> LibraryEffects {
        let mut stamped = std::mem::take(&mut self.pending_events);
        let mut recent = std::mem::take(&mut self.pending_recent_closed);
        let mut window_saves = Vec::new();
        let mut cancelled_saves = Vec::new();
        let mut pending_closes = Vec::new();
        let mut removed_watermarks = std::mem::take(&mut self.pending_removed_watermarks);
        let mut finalized_panes = std::mem::take(&mut self.pending_finalized_panes);
        for id in self.windows.iter().map(|info| info.id) {
            let Some(store) = self.stores.get_mut(&id) else {
                continue;
            };
            let mut outbox = store.take_outbox();
            removed_watermarks.append(&mut outbox.removed_watermarks);
            finalized_panes.append(&mut outbox.finalized_panes);
            pending_closes.extend(
                std::mem::take(&mut outbox.pending_closes)
                    .into_iter()
                    .map(|(record, grace)| PendingCloseTimer {
                        window: id,
                        record,
                        grace,
                    }),
            );
            if outbox.cancel_debounced_save {
                cancelled_saves.push(id);
            }
            if outbox.save != SaveRequest::None {
                window_saves.push((id, outbox.save));
            }
            recent.extend(outbox.take_stamped_recent_closed());
            stamped.extend(
                outbox
                    .into_stamped_events()
                    .map(|(emission, event)| (emission, WindowEvent { window: id, event })),
            );
        }
        stamped.sort_by_key(|(emission, _)| *emission);
        recent.sort_by_key(|(emission, _)| *emission);
        LibraryEffects {
            events: stamped.into_iter().map(|(_, event)| event).collect(),
            window_saves,
            index_save: std::mem::take(&mut self.index_dirty),
            pending_closes,
            cancelled_saves,
            recent_closed: recent.into_iter().map(|(_, change)| change).collect(),
            removed_watermarks,
            finalized_panes,
        }
    }

    /// Records pane identities removed for good outside any open store (a closed window deleted from its file), for
    /// the next drain.
    pub fn note_finalized_panes(&mut self, panes: impl IntoIterator<Item = Uuid>) {
        self.pending_finalized_panes.extend(panes);
    }

    // MARK: - Pending closes and Open Recent (upstream's `WindowLibrary+RecentClosed.swift`)

    /// Makes every open window's pending closes final (before a teardown).
    pub fn finalize_all_pending_closes(&mut self) {
        for store in self.stores.values_mut() {
            store.finalize_all_pending_closes();
        }
    }

    /// The open window holding `session`, in its tree or in a pending close, in window order.
    #[must_use]
    pub fn window_holding_session(&self, session: Uuid) -> Option<Uuid> {
        self.open_ids().into_iter().find(|id| {
            self.stores
                .get(id)
                .is_some_and(|store| store.session(session).is_some() || store.pending_close_session(session).is_some())
        })
    }

    /// The live object of `session` wherever an open window holds it: in its tree, or in a pending close whose
    /// terminals still run.
    #[must_use]
    pub fn held_session(&self, session: Uuid) -> Option<&Session> {
        let store = self.stores.get(&self.window_holding_session(session)?)?;
        store.session(session).or_else(|| store.pending_close_session(session))
    }

    /// The live object of `session` wherever an open window holds it: in its tree, or in a pending close whose
    /// terminals still run (so their exits and reports land on the object an undo brings back).
    pub fn held_session_mut(&mut self, session: Uuid) -> Option<&mut Session> {
        let window = self.window_holding_session(session)?;
        let store = self.stores.get_mut(&window)?;
        if store.session(session).is_some() {
            store.session_mut(session)
        } else {
            store.pending_close_session_mut(session)
        }
    }

    /// The Open Recent entries, newest first.
    #[must_use]
    pub fn recent_closed_items(&self) -> Vec<RecentClosedItem> {
        self.recent_closed.items()
    }

    /// Replaces the Open Recent entries with what was read from disk; records no change.
    pub fn set_recent_closed_items(&mut self, items: Vec<RecentClosedItem>) {
        self.recent_closed.replace(items);
    }

    /// Empties Open Recent.
    pub fn clear_recent_closed_items(&mut self) {
        self.recent_closed.clear();
        self.push_recent_closed_change(RecentClosedChange::Clear);
    }

    fn push_recent_closed_change(&mut self, change: RecentClosedChange) {
        self.pending_recent_closed.push((next_emission(), change));
    }

    /// Reopens the entry `item`; whether anything was restored. See
    /// [`WindowLibrary::reopen_recent_closed_reporting_window`].
    pub fn reopen_recent_closed(&mut self, item: Uuid, into: Option<Uuid>) -> bool {
        self.reopen_recent_closed_reporting_window(item, into).is_some()
    }

    /// Reopens the newest entry; whether anything was restored.
    pub fn reopen_latest_recent_closed(&mut self, into: Option<Uuid>) -> bool {
        self.reopen_latest_recent_closed_reporting_window(into).is_some()
    }

    /// [`WindowLibrary::reopen_recent_closed_reporting_window`] over the newest entry.
    pub fn reopen_latest_recent_closed_reporting_window(&mut self, into: Option<Uuid>) -> Option<Uuid> {
        let item = self.recent_closed.first()?;
        self.reopen_recent_closed_reporting_window(item.id, into)
    }

    /// Reopens the entry `item` and answers the window it went to, for the caller to reveal; `None` when nothing was
    /// restored. That window is the one holding the entry's session, else the one holding its workspace, else `into`
    /// (an open window), else the active one: a second object under one id would make every id-keyed lookup answer
    /// with whichever window sorts first. The entry survives an incomplete restore, so a member left in no window
    /// can still be reopened.
    pub fn reopen_recent_closed_reporting_window(&mut self, item: Uuid, into: Option<Uuid>) -> Option<Uuid> {
        let item = self.recent_closed.item(item)?;
        let window = self.restore_destination(&item, into)?;
        let occupied = self.session_ids_held_outside(window);
        let outcome = self
            .stores
            .get_mut(&window)?
            .restore_recent_closed_with(&item, &occupied);
        if !outcome.restored {
            return None;
        }
        if outcome.complete {
            self.recent_closed.remove(item.id);
            self.push_recent_closed_change(RecentClosedChange::Remove(item.id));
        }
        Some(window)
    }

    /// Which window restores `item`: the one holding the session itself, then the one holding its workspace, then
    /// the requested one. Session first, since an old snapshot can leave one session in two windows, where the stale
    /// workspace would rebuild the copy just closed.
    fn restore_destination(&self, item: &RecentClosedItem, requested: Option<Uuid>) -> Option<Uuid> {
        if item.kind == RecentClosedKind::Session
            && let Some(owner) = item
                .session
                .as_ref()
                .and_then(|recent| self.window_holding_session(recent.snapshot.id))
        {
            return Some(owner);
        }
        self.window_holding_workspace(item)
            .or_else(|| requested.filter(|id| self.is_open(*id)))
            .or_else(|| self.active_window_id())
    }

    /// The open window holding `item`'s workspace (a workspace entry's own, a session entry's parent), in its tree or
    /// in a pending close.
    fn window_holding_workspace(&self, item: &RecentClosedItem) -> Option<Uuid> {
        let workspace = match item.kind {
            RecentClosedKind::Session => item.session.as_ref().map(|recent| recent.workspace_id),
            RecentClosedKind::Workspace => item.workspace.as_ref().map(|recent| recent.snapshot.id),
        }?;
        self.open_ids().into_iter().find(|id| {
            self.stores
                .get(id)
                .is_some_and(|store| store.workspace(workspace).is_some() || store.pending_holds_workspace(workspace))
        })
    }

    /// Every session id an open window other than `window` holds, in its tree or in a pending close: the restoring
    /// window must rebuild none of them.
    fn session_ids_held_outside(&self, window: Uuid) -> HashSet<Uuid> {
        let mut held = HashSet::new();
        for (_, store) in self.stores.iter().filter(|(id, _)| **id != window) {
            held.extend(store.sessions().map(Session::id));
            held.extend(store.pending_held_session_ids());
        }
        held
    }
}
