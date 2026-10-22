//! A session's in-terminal search (upstream's `Session.search*` fields and the factory's `wireSearchCallbacks`): the
//! pane that owns the open search, the needle, and libghostty's match count and selection. Ephemeral.

use super::pane::StatusPane;

/// What a terminal reported about its search (libghostty's `START_SEARCH`, `END_SEARCH`, `SEARCH_TOTAL` and
/// `SEARCH_SELECTED` actions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchReport {
    /// Search mode began, with the needle libghostty seeded it with, if any.
    Started(Option<String>),
    /// Search mode ended.
    Ended,
    /// The match count of the current needle; `None` with no query.
    Total(Option<usize>),
    /// The 1-based index of the selected match; `None` with none selected.
    Selected(Option<usize>),
}

/// What a report asks of the frontend beyond the state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFollowUp {
    /// Nothing.
    None,
    /// A second start while a search is open is the find-again close: end search on the owning pane, whose
    /// `END_SEARCH` report then clears the state.
    EndOn(StatusPane),
}

/// A session's search state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchState {
    /// Whether a search is open (a `START_SEARCH` landed and no `END_SEARCH` since).
    pub active: bool,
    /// The pane owning the open search, pinned at its start so a focus move cannot retarget it.
    pub owner: Option<StatusPane>,
    /// The query, mirrored from the control channel and the start report.
    pub needle: String,
    /// libghostty's match count for `needle`; `None` before a query ran.
    pub total: Option<usize>,
    /// The 1-based selected match; `None` while none is.
    pub selected: Option<usize>,
}

impl SearchState {
    /// The match counter (upstream's `searchDisplayText`): empty before a query runs, `no matches` at zero,
    /// `N matches` while none is selected, `S of N` once one is, the selection clamped to the total.
    #[must_use]
    pub fn display_text(&self) -> String {
        match (self.total, self.selected) {
            (None, _) => String::new(),
            (Some(0), _) => "no matches".to_owned(),
            (Some(total), None) => format!("{total} matches"),
            (Some(total), Some(selected)) => format!("{} of {total}", selected.min(total)),
        }
    }

    /// Resets every field, as a pane teardown or promotion does (upstream's `clearSearch`).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Lands `report` from the terminal in pane `pane`.
    pub fn apply(&mut self, pane: StatusPane, report: SearchReport) -> SearchFollowUp {
        match report {
            SearchReport::Started(needle) => {
                if self.active {
                    return self.owner.map_or(SearchFollowUp::None, SearchFollowUp::EndOn);
                }
                self.active = true;
                self.owner = Some(pane);
                if let Some(needle) = needle.filter(|needle| !needle.is_empty()) {
                    self.needle = needle;
                }
            }
            SearchReport::Ended => self.clear(),
            SearchReport::Total(total) => self.total = total,
            SearchReport::Selected(selected) => self.selected = selected,
        }
        SearchFollowUp::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counter_reads_as_upstreams() {
        let mut state = SearchState::default();
        assert_eq!(state.display_text(), "");
        state.total = Some(0);
        assert_eq!(state.display_text(), "no matches");
        state.total = Some(3);
        assert_eq!(state.display_text(), "3 matches");
        state.selected = Some(2);
        assert_eq!(state.display_text(), "2 of 3");
        state.selected = Some(5);
        assert_eq!(state.display_text(), "3 of 3");
    }

    #[test]
    fn a_start_pins_its_pane_and_a_second_start_ends_on_the_owner() {
        let mut state = SearchState::default();
        assert_eq!(
            state.apply(StatusPane::Right, SearchReport::Started(Some("seed".to_owned()))),
            SearchFollowUp::None
        );
        assert_eq!(state.owner, Some(StatusPane::Right));
        assert_eq!(state.needle, "seed");
        assert_eq!(
            state.apply(StatusPane::Left, SearchReport::Started(None)),
            SearchFollowUp::EndOn(StatusPane::Right)
        );
        state.apply(StatusPane::Right, SearchReport::Total(Some(4)));
        state.apply(StatusPane::Right, SearchReport::Selected(Some(1)));
        assert_eq!(state.display_text(), "1 of 4");
        state.apply(StatusPane::Right, SearchReport::Ended);
        assert_eq!(state, SearchState::default());
    }
}
