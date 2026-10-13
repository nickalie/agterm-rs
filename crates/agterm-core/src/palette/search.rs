//! How palette rows are searched (upstream's `PaletteSearchKeys.swift`, `PaletteCustomRow.swift` and the ranking in
//! `CommandPalette.updateFiltered`).

use super::{PaletteEntry, PaletteMode};
use crate::fuzzy::fuzzy_rank;

/// The search keys of one palette row (upstream's `paletteSearchKeys`). A caller-supplied picker matches the label
/// only, so a subtitle can carry consequence text ("cannot be undone") without a refusal word isolating a destructive
/// row; the built-in palettes match their subtitle too, which carries the workspace and detail a user searches by.
#[must_use]
pub fn palette_search_keys(title: &str, subtitle: Option<&str>, caller_supplied: bool) -> Vec<String> {
    match subtitle {
        Some(subtitle) if !caller_supplied => vec![title.to_owned(), subtitle.to_owned()],
        _ => vec![title.to_owned()],
    }
}

/// The free-text row a picker offers when its query matches no item (upstream's `pickCustomRowLabel`): `Use "<query>"`
/// for a non-blank query with custom answers allowed, the query trimmed of whitespace and newlines.
#[must_use]
pub fn pick_custom_row_label(query: &str, filtered_count: usize, allow_custom: bool) -> Option<String> {
    let value = query.trim();
    (allow_custom && filtered_count == 0 && !value.is_empty()).then(|| format!("Use \"{value}\""))
}

/// The rows `query` shows of a built-in palette's `entries`, as indices into them (upstream's `updateFiltered`): the
/// matching rows best first, ties A→Z by title; a blank query (trimmed of whitespace and newlines) lists everything
/// A→Z, except the attention palette, which keeps its source order so Return runs its first row.
#[must_use]
pub fn filter_entries(entries: &[PaletteEntry], query: &str, mode: PaletteMode) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() && mode == PaletteMode::Attention {
        return (0..entries.len()).collect();
    }
    fuzzy_rank(query, 0..entries.len(), |index| {
        entries
            .get(*index)
            .map(|entry| palette_search_keys(&entry.title, entry.subtitle.as_deref(), false))
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::PaletteAction;
    use crate::palette_catalog::PaletteCommand;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row {
        title: &'static str,
        subtitle: Option<&'static str>,
    }

    const CONFIRM_ROWS: [Row; 2] = [
        Row {
            title: "Confirm",
            subtitle: Some("cannot be undone"),
        },
        Row {
            title: "Cancel",
            subtitle: None,
        },
    ];

    fn rank(query: &str, caller_supplied: bool) -> Vec<Row> {
        fuzzy_rank(query, CONFIRM_ROWS, |row| {
            palette_search_keys(row.title, row.subtitle, caller_supplied)
        })
    }

    // pins the confirm-row trap: "cannot be undone" matched the refusal query "no" and left the destructive row alone
    // and preselected for Return.
    #[test]
    fn refusal_query_leaves_no_row_in_a_caller_supplied_confirm() {
        assert!(rank("no", true).is_empty());
    }

    #[test]
    fn refusal_query_still_matches_a_subtitle_in_a_builtin_palette() {
        assert_eq!(rank("no", false), CONFIRM_ROWS[..1]);
    }

    #[test]
    fn label_still_matches_in_a_caller_supplied_picker() {
        assert_eq!(rank("can", true), CONFIRM_ROWS[1..]);
    }

    #[test]
    fn caller_supplied_row_matches_its_label_only() {
        assert_eq!(
            palette_search_keys("Confirm", Some("cannot be undone"), true),
            ["Confirm"]
        );
    }

    #[test]
    fn builtin_row_matches_label_and_subtitle() {
        assert_eq!(
            palette_search_keys("Go to Session", Some("work · zsh"), false),
            ["Go to Session", "work · zsh"]
        );
    }

    #[test]
    fn builtin_row_without_subtitle_matches_its_label_only() {
        assert_eq!(palette_search_keys("New Window", None, false), ["New Window"]);
    }

    #[test]
    fn the_custom_row_needs_a_blank_free_unmatched_query_and_permission() {
        assert_eq!(
            pick_custom_row_label("  zzz top \n", 0, true).as_deref(),
            Some("Use \"zzz top\"")
        );
        assert_eq!(pick_custom_row_label("zzz", 0, false), None);
        assert_eq!(pick_custom_row_label("zzz", 1, true), None);
        assert_eq!(pick_custom_row_label(" \n\t", 0, true), None);
    }

    fn entry(title: &str, subtitle: Option<&str>) -> PaletteEntry {
        PaletteEntry {
            id: title.to_owned(),
            title: title.to_owned(),
            subtitle: subtitle.map(str::to_owned),
            shortcut: None,
            badge: None,
            indicator: None,
            action: PaletteAction::Command(PaletteCommand::NewSession),
        }
    }

    #[test]
    fn a_blank_query_lists_a_builtin_palette_a_to_z_but_keeps_the_attention_order() {
        let entries = [entry("beta", None), entry("Alpha", None), entry("gamma", None)];
        assert_eq!(filter_entries(&entries, "", PaletteMode::Actions), [1, 0, 2]);
        assert_eq!(filter_entries(&entries, " \n", PaletteMode::Sessions), [1, 0, 2]);
        assert_eq!(filter_entries(&entries, "  ", PaletteMode::Attention), [0, 1, 2]);
    }

    #[test]
    fn a_query_ranks_titles_and_subtitles_best_first() {
        let entries = [
            entry("zsh", Some("work · ~/src")),
            entry("New Session", None),
            entry("build", Some("ops · zsh")),
        ];
        assert_eq!(
            filter_entries(&entries, "zsh", PaletteMode::Sessions),
            [0, 2],
            "a title prefix ahead of a subtitle substring"
        );
        assert_eq!(filter_entries(&entries, "ops", PaletteMode::Attention), [2]);
        assert_eq!(filter_entries(&entries, "work", PaletteMode::Sessions), [0]);
        assert!(filter_entries(&entries, "xyz", PaletteMode::Actions).is_empty());
    }
}
