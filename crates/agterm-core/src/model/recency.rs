//! The most-recently-used session order (upstream's `RecencyStack.swift`): the Ctrl-Tab switcher's order, the MRU
//! reselection after a close, and `dashboard --mru`.

use std::collections::HashSet;
use std::hash::Hash;

/// The default bound of a recency list.
pub const RECENCY_LIMIT: usize = 100;

/// A bounded most-recent-first list of unique ids: `items()[0]` is the current one, `items()[1]` the previous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecencyStack<Id> {
    items: Vec<Id>,
    limit: usize,
}

impl<Id: Copy + Eq + Hash> Default for RecencyStack<Id> {
    fn default() -> Self {
        Self::new(RECENCY_LIMIT, Vec::new())
    }
}

impl<Id: Copy + Eq + Hash> RecencyStack<Id> {
    /// Seeds the list from `items`, keeping the first occurrence of each id (a persisted or hand-edited seed may
    /// repeat one) and truncating to `limit`.
    #[must_use]
    pub fn new(limit: usize, items: impl IntoIterator<Item = Id>) -> Self {
        let mut seen = HashSet::new();
        let items = items.into_iter().filter(|id| seen.insert(*id)).take(limit).collect();
        Self { items, limit }
    }

    /// The ids, most recent first.
    #[must_use]
    pub fn items(&self) -> &[Id] {
        &self.items
    }

    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Moves `id` to the front; a no-op when it already is.
    pub fn push(&mut self, id: Id) {
        if self.items.first() == Some(&id) {
            return;
        }
        self.items.retain(|item| *item != id);
        self.items.insert(0, id);
        self.items.truncate(self.limit);
    }

    pub fn remove(&mut self, id: Id) {
        self.items.retain(|item| *item != id);
    }

    /// Up to `n` most recent ids that `valid` accepts, most recent first. Stale ids are skipped, so a closed session
    /// never shows even before it is removed.
    #[must_use]
    pub fn top(&self, n: usize, valid: impl Fn(&Id) -> bool) -> Vec<Id> {
        self.items.iter().copied().filter(|id| valid(id)).take(n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_moves_to_front_without_duplicates() {
        let mut stack = RecencyStack::default();
        stack.push(1);
        stack.push(2);
        stack.push(3);
        assert_eq!(stack.items(), [3, 2, 1]);
        stack.push(1);
        assert_eq!(stack.items(), [1, 3, 2]);
        stack.push(1);
        assert_eq!(stack.items(), [1, 3, 2]);
    }

    #[test]
    fn push_truncates_to_the_limit() {
        let mut stack = RecencyStack::new(3, []);
        for id in 1..=5 {
            stack.push(id);
        }
        assert_eq!(stack.items(), [5, 4, 3]);
    }

    #[test]
    fn remove_drops_the_id() {
        let mut stack = RecencyStack::new(10, [1, 2, 3]);
        stack.remove(2);
        assert_eq!(stack.items(), [1, 3]);
        stack.remove(9);
        assert_eq!(stack.items(), [1, 3]);
    }

    #[test]
    fn top_skips_stale_ids_and_stops_at_n() {
        let stack = RecencyStack::new(10, [4, 3, 2, 1]);
        let valid = [1, 2, 4];
        assert_eq!(stack.top(2, |id| valid.contains(id)), [4, 2]);
        assert_eq!(stack.top(10, |id| valid.contains(id)), [4, 2, 1]);
        assert!(stack.top(0, |_| true).is_empty());
        assert!(stack.top(3, |_| false).is_empty());
    }

    #[test]
    fn a_seed_keeps_first_occurrences_and_truncates() {
        let stack = RecencyStack::new(3, [1, 2, 1, 3, 2, 4]);
        assert_eq!(stack.items(), [1, 2, 3]);
        assert!(RecencyStack::<u8>::default().is_empty());
        assert_eq!(RecencyStack::<u8>::default().limit(), RECENCY_LIMIT);
    }
}
