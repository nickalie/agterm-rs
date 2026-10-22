//! The fuzzy matching of the command palettes and the native picker (upstream's `Fuzzy.swift`).

use std::cmp::Ordering;

/// The substring band's cap: a single term's literal match (39 at worst) stays ahead of any subsequence-only match.
const SUBSTRING_OFFSET_CAP: usize = 34;
const SUBSTRING_BASE: usize = 5;
const SUBSEQUENCE_BASE: usize = 40;

/// How well `query` matches `target`, lower is better, `None` for no match (upstream's `fuzzyScore`).
///
/// The query splits on whitespace into terms; every term must match and the score is their sum, so term order is
/// irrelevant. An empty or whitespace-only query matches everything at 0. Case-insensitive. Per term: a prefix is 0,
/// a substring `5 + min(offset, 34)`, a scattered subsequence `40 +` the length gap.
#[must_use]
pub fn fuzzy_score(query: &str, target: &str) -> Option<usize> {
    let query = query.to_lowercase();
    let target = target.to_lowercase();
    query
        .split_whitespace()
        .try_fold(0, |total, term| term_score(term, &target).map(|score| total + score))
}

/// Ranks `items` against `query` (upstream's `fuzzyRank`): an item matches when any of its `keys` (a title and an
/// optional subtitle, say) matches, scored by its best key; best first, ties broken by the first key
/// case-insensitively. An empty query scores every item 0, which makes this an alphabetical sort.
pub fn fuzzy_rank<T, K>(query: &str, items: impl IntoIterator<Item = T>, keys: K) -> Vec<T>
where
    K: Fn(&T) -> Vec<String>,
{
    let mut scored: Vec<(T, usize, String)> = items
        .into_iter()
        .filter_map(|item| {
            let item_keys = keys(&item);
            let best = item_keys.iter().filter_map(|key| fuzzy_score(query, key)).min()?;
            let label = item_keys.into_iter().next().unwrap_or_default();
            Some((item, best, label))
        })
        .collect();
    scored.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| case_insensitive(&a.2, &b.2)));
    scored.into_iter().map(|(item, ..)| item).collect()
}

fn case_insensitive(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// One whitespace-free term against the lower-cased target.
fn term_score(term: &str, target: &str) -> Option<usize> {
    if target.starts_with(term) {
        return Some(0);
    }
    if let Some(byte_offset) = target.find(term) {
        let offset = target[..byte_offset].chars().count();
        return Some(SUBSTRING_BASE + offset.min(SUBSTRING_OFFSET_CAP));
    }
    // a subsequence: every term character appears in order, not necessarily adjacent
    let mut wanted = term.chars().peekable();
    for ch in target.chars() {
        if wanted.peek() == Some(&ch) {
            wanted.next();
            if wanted.peek().is_none() {
                let gap = target.chars().count().saturating_sub(term.chars().count());
                return Some(SUBSEQUENCE_BASE + gap);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rank(query: &str, items: &[&'static str]) -> Vec<&'static str> {
        fuzzy_rank(query, items.iter().copied(), |item| vec![(*item).to_owned()])
    }

    #[test]
    fn empty_and_whitespace_queries_match_everything_at_zero() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("  \t ", "anything"), Some(0));
    }

    #[test]
    fn a_prefix_scores_zero_case_insensitively() {
        assert_eq!(fuzzy_score("new", "New Session"), Some(0));
        assert_eq!(fuzzy_score("NEW", "new session"), Some(0));
    }

    #[test]
    fn a_substring_scores_by_its_offset_capped_at_39() {
        assert_eq!(fuzzy_score("session", "New Session"), Some(5 + 4));
        assert_eq!(fuzzy_score("z", &format!("{}z", "a".repeat(40))), Some(39));
        assert_eq!(fuzzy_score("z", &format!("{}z", "a".repeat(80))), Some(39));
    }

    #[test]
    fn a_subsequence_scores_above_any_substring_and_a_miss_is_none() {
        assert!(fuzzy_score("nsn", "New Session").is_some_and(|score| score >= 40));
        assert_eq!(fuzzy_score("xyz", "New Session"), None);
        let prefix = fuzzy_score("ne", "New Session").unwrap();
        let substring = fuzzy_score("ew", "New Session").unwrap();
        let subsequence = fuzzy_score("nss", "New Session").unwrap();
        assert!(prefix < substring && substring < subsequence);
    }

    #[test]
    fn terms_match_independently_and_all_must_match() {
        assert!(fuzzy_score("cap dev", "caprica-dev").is_some());
        assert_eq!(
            fuzzy_score("cap dev", "caprica-dev"),
            fuzzy_score("dev cap", "caprica-dev")
        );
        assert_eq!(fuzzy_score("cap xyz", "caprica-dev"), None);
        assert!(fuzzy_score("cap   dev", "caprica-dev").is_some());
    }

    #[test]
    fn a_far_substring_outranks_a_near_subsequence() {
        let stems = "~/Music/Ableton/Exports/2026/album/bounces/stems";
        assert_eq!(rank("stem", &["/opt/steam", stems]), vec![stems, "/opt/steam"]);
    }

    #[test]
    fn ranking_sorts_best_first_and_drops_misses() {
        assert_eq!(
            rank("new", &["New Workspace", "New Session", "Close Session"]),
            vec!["New Session", "New Workspace"]
        );
        assert_eq!(
            rank("sess", &["New Session", "Session List"]),
            vec!["Session List", "New Session"]
        );
        assert_eq!(rank("", &["beta", "Alpha", "gamma"]), vec!["Alpha", "beta", "gamma"]);
        assert_eq!(rank("ps", &["Split Pane", "Panel Split"]), vec!["Panel Split"]);
    }

    #[test]
    fn ranking_takes_the_best_of_every_key() {
        let items = [("Toggle Split", "panes"), ("Reload", "split config")];
        let ranked = fuzzy_rank("split", items, |(title, subtitle)| {
            vec![(*title).to_owned(), (*subtitle).to_owned()]
        });
        assert_eq!(ranked, vec![("Reload", "split config"), ("Toggle Split", "panes")]);
    }
}
