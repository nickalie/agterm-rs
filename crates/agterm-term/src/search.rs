//! Scrollback search: what libghostty's search actions did for the GTK build (`search:NEEDLE`, `navigate_search`,
//! `end_search`, and the `SEARCH_TOTAL`/`SEARCH_SELECTED` reports), on libghostty-vt's search.
//!
//! The search lives in its own mutex, after the VT lock in the pane's lock order (VT → search). Its calls that read
//! the terminal (create, set the needle, feed, select, free) run under the VT lock too; a worker thread, alive while
//! a needle is set, feeds it every [`REFRESH`] (as Ghostty's search thread does) and ticks it with only the search's
//! mutex held, so a long scrollback is searched without stalling output or paint. Counts and the selected match go to
//! the frontend as [`PaneEvent::SearchTotal`] and [`PaneEvent::SearchSelected`] when they change.

use std::sync::{Arc, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use agterm_ghostty_vt as vt;

use crate::effects::PaneEvent;
use crate::pane::{Pane, Shared, lock};

/// How often a running search catches up with the terminal (Ghostty's `REFRESH_INTERVAL`, 24 ms).
const REFRESH: Duration = Duration::from_millis(24);
/// Ticks between releases of the search's mutex.
const TICKS: usize = 64;

/// Which way [`Pane::navigate_search`] steps, named as Ghostty's `navigate_search` action names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchStep {
    /// `navigate_search:next`: toward older output, up into the scrollback (wraps).
    Next,
    /// `navigate_search:previous`: toward newer output (wraps).
    Previous,
}

/// A match on the viewport: its first and last cell (column, row), clipped to the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Highlight {
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl Highlight {
    fn from_engine(found: vt::Match) -> Self {
        let cell = |point: vt::Point| (point.x, u16::try_from(point.y).unwrap_or(u16::MAX));
        Self {
            start: cell(found.start),
            end: cell(found.end),
        }
    }
}

/// A pane's search as of the last time it caught up with the terminal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchState {
    /// The needle; empty with no search.
    pub needle: String,
    /// The matches found so far; `None` with no search.
    pub total: Option<usize>,
    /// The selected match's index, newest first (0 is the match nearest the bottom), as libghostty's
    /// `SEARCH_SELECTED` reported it; `None` while none is selected.
    pub selected: Option<usize>,
    /// The matches on the viewport, for highlighting.
    pub highlights: Vec<Highlight>,
    /// The selected match, when it is on the viewport.
    pub selected_highlight: Option<Highlight>,
}

/// The search's mutex.
#[derive(Default)]
pub(crate) struct SearchSlot {
    search: Option<vt::Search>,
    state: SearchState,
    /// What the frontend was last told.
    reported: (Option<usize>, Option<usize>),
    worker: Option<JoinHandle<()>>,
    /// A worker runs (it clears this, under the mutex, when it decides to end).
    running: bool,
    /// Bumped by every needle change and stop, so a worker of an older search ends.
    epoch: u64,
    stopping: bool,
}

impl Pane {
    /// Search for `needle` (Ghostty's `search:NEEDLE`): the same needle keeps the results, a new one restarts, an
    /// empty one ends the search. Whether anything was done (an empty needle with no search is not).
    pub fn search(&self, needle: &str) -> bool {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let mut slot = lock(&shared.search);
        if needle.is_empty() {
            let Some(search) = slot.search.take() else {
                return false;
            };
            if let Err(err) = search.close(&mut vt.terminal) {
                tracing::warn!(%err, "closing a search failed");
            }
            slot.epoch += 1;
            slot.state = SearchState::default();
            // `SearchTotal(None)` (and `SearchSelected(None)` after a selection), as libghostty reported the end
            report(shared, &mut slot);
            shared.search_wake.notify_all();
            drop(slot);
            drop(vt);
            shared.wake();
            return true;
        }
        if slot.search.is_none() {
            match vt::Search::new(&mut vt.terminal) {
                Ok(search) => slot.search = Some(search),
                Err(err) => {
                    tracing::warn!(%err, "starting a search failed");
                    return false;
                }
            }
        }
        if let Some(search) = slot.search.as_mut()
            && let Err(err) = search.set_needle(&mut vt.terminal, needle)
        {
            tracing::warn!(%err, "setting the search needle failed");
        }
        needle.clone_into(&mut slot.state.needle);
        slot.epoch += 1;
        drop(vt);
        start_worker(shared, slot);
        true
    }

    /// Select the next match in `step`'s direction, scrolling it into view (Ghostty's `navigate_search`). Whether a
    /// search is open.
    pub fn navigate_search(&self, step: SearchStep) -> bool {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let mut slot = lock(&shared.search);
        let SearchSlot { search, state, .. } = &mut *slot;
        let Some(search) = search.as_mut() else {
            return false;
        };
        let direction = match step {
            SearchStep::Next => vt::SearchDirection::Older,
            SearchStep::Previous => vt::SearchDirection::Newer,
        };
        match search.select(&mut vt.terminal, direction) {
            Ok(Some(results)) => refresh(&results, state),
            // no match to select
            Ok(None) => {}
            Err(err) => tracing::warn!(%err, "selecting a search match failed"),
        }
        drop(vt);
        report(shared, &mut slot);
        drop(slot);
        shared.wake();
        true
    }

    /// The search's needle, counts and highlights.
    #[must_use]
    pub fn search_state(&self) -> SearchState {
        lock(&self.shared().search).state.clone()
    }
}

/// Take what the search found into `state`.
fn refresh(results: &vt::SearchResults, state: &mut SearchState) {
    state.total = Some(results.total);
    state.selected = results.selected;
    state.highlights = results
        .viewport_matches
        .iter()
        .copied()
        .map(Highlight::from_engine)
        .collect();
    state.selected_highlight = results.selected_match.map(Highlight::from_engine);
}

/// Tell the frontend what changed since it was last told.
fn report(shared: &Shared, slot: &mut SearchSlot) {
    let (total, selected) = (slot.state.total, slot.state.selected);
    if slot.reported.0 != total {
        shared.send(PaneEvent::SearchTotal(total));
    }
    if slot.reported.1 != selected {
        shared.send(PaneEvent::SearchSelected(selected));
    }
    slot.reported = (total, selected);
}

/// Start the worker unless one runs.
fn start_worker(shared: &Arc<Shared>, mut slot: MutexGuard<'_, SearchSlot>) {
    shared.search_wake.notify_all();
    if slot.running || slot.stopping {
        return;
    }
    let epoch = slot.epoch;
    let worker = {
        let shared = Arc::clone(shared);
        std::thread::Builder::new()
            .name("pane-search".to_owned())
            .spawn(move || run(&shared, epoch))
    };
    match worker {
        Ok(worker) => {
            slot.running = true;
            // a previous worker's handle is finished or finishing (it cleared `running` before ending)
            slot.worker = Some(worker);
        }
        Err(err) => tracing::warn!(%err, "cannot start the search thread"),
    }
}

/// The worker: catch up, tick, report, wait; until the search ends or the pane goes.
fn run(shared: &Shared, mut epoch: u64) {
    loop {
        let Some(status) = feed(shared) else {
            return;
        };
        let status = if status == vt::SearchStatus::Running {
            tick(shared)
        } else {
            status
        };
        let mut slot = lock(&shared.search);
        if slot.stopping || slot.search.is_none() {
            slot.running = false;
            return;
        }
        report(shared, &mut slot);
        drop(slot);
        shared.wake();
        if status == vt::SearchStatus::Complete {
            let slot = lock(&shared.search);
            let (slot, _) = shared
                .search_wake
                .wait_timeout_while(slot, REFRESH, |slot| slot.epoch == epoch && !slot.stopping)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            epoch = slot.epoch;
        }
    }
}

/// Feed the search under the VT lock and take its results; `None` when there is no search any more.
fn feed(shared: &Shared) -> Option<vt::SearchStatus> {
    let mut vt = lock(&shared.vt);
    let mut slot = lock(&shared.search);
    let SearchSlot {
        search,
        state,
        running,
        stopping,
        ..
    } = &mut *slot;
    let Some(search) = search.as_mut().filter(|_| !*stopping) else {
        *running = false;
        return None;
    };
    match search.feed(&mut vt.terminal) {
        Ok(results) => {
            refresh(&results, state);
            Some(results.status)
        }
        Err(err) => {
            tracing::warn!(%err, "feeding the search failed");
            *running = false;
            None
        }
    }
}

/// Tick with only the search's mutex, releasing it every [`TICKS`] ticks; the status reached. The counts it reached
/// are taken right away (they need no terminal); the highlights wait for the next feed.
fn tick(shared: &Shared) -> vt::SearchStatus {
    loop {
        let mut slot = lock(&shared.search);
        let SearchSlot { search, state, .. } = &mut *slot;
        let Some(search) = search.as_mut() else {
            return vt::SearchStatus::Complete;
        };
        for _ in 0..TICKS {
            let status = match search.tick() {
                Ok(vt::SearchStatus::Running) => continue,
                Ok(status) => status,
                Err(_) => vt::SearchStatus::Complete,
            };
            state.total = Some(search.total());
            state.selected = search.selected_index();
            return status;
        }
    }
}

/// Stop the worker and free the search (the pane is going): the worker is joined first, then the search is closed
/// under the VT lock, before the terminal it tracks state in.
pub(crate) fn shut_down(shared: &Shared) {
    let worker = {
        let mut slot = lock(&shared.search);
        slot.stopping = true;
        shared.search_wake.notify_all();
        slot.worker.take()
    };
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    let mut vt = lock(&shared.vt);
    if let Some(search) = lock(&shared.search).search.take()
        && let Err(err) = search.close(&mut vt.terminal)
    {
        tracing::warn!(%err, "closing a search failed");
    }
}
