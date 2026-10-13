//! Ported from upstream's `HudMarkdownTests`.

use super::*;

fn plain(source: &str) -> Vec<String> {
    lines(source)
        .iter()
        .map(|line| match line.kind {
            LineKind::Rule => format!("{}<rule>", line.lead),
            _ => format!("{}{}", line.lead, joined(&line.runs)),
        })
        .collect()
}

fn joined(runs: &[Run]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

fn styled(source: &str) -> Vec<Vec<Run>> {
    lines(source).into_iter().map(|line| line.runs).collect()
}

fn laid_out(source: &str, width: usize) -> Vec<String> {
    rows(&lines(source), width).iter().map(|row| joined(row)).collect()
}

fn run(text: &str, style: Style) -> Run {
    Run::new(text, style)
}

fn none() -> Style {
    Style::empty()
}

const ESC: &str = "\u{1B}[";
const OSC: &str = "\u{1B}]";
const ST: &str = "\u{1B}\\";

#[test]
fn a_soft_break_joins_with_a_space_and_a_hard_break_starts_a_row() {
    assert_eq!(plain("one\ntwo"), ["one two"]);
    assert_eq!(plain("one  \ntwo"), ["one", "two"]);
    assert_eq!(plain("one\\\ntwo"), ["one", "two"]);
}

#[test]
fn paragraphs_are_separated_by_one_blank_row_whatever_the_source_had() {
    assert_eq!(plain("a\n\nb\n\n\n\nc"), ["a", "", "b", "", "c"]);
}

#[test]
fn every_heading_level_is_bold() {
    for level in 1..=6 {
        let lines = lines(&format!("{} Tasks", "#".repeat(level)));

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs, [run("Tasks", Style::BOLD)]);
    }
}

#[test]
fn a_heading_keeps_its_bold_after_an_inner_strong_span() {
    assert_eq!(
        styled("# **x** y"),
        [vec![run("x", Style::BOLD), run(" y", Style::BOLD)]]
    );
}

#[test]
fn unordered_items_get_a_bullet_and_hang_their_continuation() {
    let lines = lines("- build\n- test");

    assert_eq!(
        lines.iter().map(|line| line.lead.as_str()).collect::<Vec<_>>(),
        ["• ", "• "]
    );
    assert_eq!(
        lines.iter().map(|line| line.hang.as_str()).collect::<Vec<_>>(),
        ["  ", "  "]
    );
    assert_eq!(plain("- build\n- test"), ["• build", "• test"]);
}

#[test]
fn ordered_items_keep_their_ordinal_and_hang_by_its_width() {
    let lines = lines("9. nine\n10. ten");

    assert_eq!(
        lines.iter().map(|line| line.lead.as_str()).collect::<Vec<_>>(),
        ["9. ", "10. "]
    );
    assert_eq!(
        lines.iter().map(|line| line.hang.as_str()).collect::<Vec<_>>(),
        ["   ", "    "]
    );
}

#[test]
fn a_nested_list_indents_under_its_parents_text() {
    assert_eq!(
        plain("- parent\n  - child\n    - grandchild"),
        ["• parent", "  • child", "    • grandchild"]
    );
}

#[test]
fn tight_and_loose_lists_render_alike() {
    assert_eq!(plain("- a\n- b"), ["• a", "• b"]);
    assert_eq!(plain("- a\n\n- b"), ["• a", "• b"]);
}

#[test]
fn a_list_is_separated_from_the_paragraph_around_it() {
    assert_eq!(
        plain("status\n\n- a\n- b\n\ndone"),
        ["status", "", "• a", "• b", "", "done"]
    );
}

#[test]
fn a_task_checkbox_stays_literal_text() {
    assert_eq!(plain("- [ ] queued\n- [x] done"), ["• [ ] queued", "• [x] done"]);
}

#[test]
fn a_code_block_keeps_its_lines_verbatim_and_indented() {
    let lines = lines("```\n# not a heading\n- not a bullet\n**not bold**\n\n\tx\n```");

    assert_eq!(
        lines
            .iter()
            .map(|line| format!("{}{}", line.lead, joined(&line.runs)))
            .collect::<Vec<_>>(),
        [
            "  # not a heading",
            "  - not a bullet",
            "  **not bold**",
            "  ",
            "      x"
        ]
    );
    assert!(lines.iter().flat_map(|line| &line.runs).all(|run| run.style.is_empty()));
}

#[test]
fn tabs_expand_to_the_next_stop() {
    assert_eq!(expand_tabs("a\tb"), "a   b");
    assert_eq!(expand_tabs("abcd\te"), "abcd    e");
}

#[test]
fn a_block_quote_prefixes_every_row() {
    assert_eq!(plain("> first  \n> second"), ["│ first", "│ second"]);
}

#[test]
fn a_thematic_break_is_a_rule_row() {
    let lines = lines("above\n\n---\n\nbelow");

    assert_eq!(
        lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
        [
            LineKind::Text,
            LineKind::Text,
            LineKind::Rule,
            LineKind::Text,
            LineKind::Text
        ]
    );
}

#[test]
fn a_table_is_framed_with_a_padded_grid_and_a_bold_header() {
    let source = "| task | state |\n|---|---|\n| build | ok |\n| lint **x** | running |";
    let lines = lines(source);

    assert_eq!(
        plain(source),
        [
            "┌────────┬─────────┐",
            "│ task   │ state   │",
            "├────────┼─────────┤",
            "│ build  │ ok      │",
            "│ lint x │ running │",
            "└────────┴─────────┘",
        ]
    );
    assert!(
        lines[1]
            .runs
            .iter()
            .filter(|run| run.text.chars().any(char::is_alphabetic))
            .all(|run| run.style.contains(Style::BOLD))
    );
    assert!(
        lines[1]
            .runs
            .iter()
            .filter(|run| run.text.contains('│'))
            .all(|run| run.style.is_empty())
    );
    assert!(lines[4].runs.contains(&run("x", Style::BOLD)));
    assert!(lines.iter().all(|line| width(&line.runs) == width(&lines[0].runs)));
}

#[test]
fn an_all_empty_header_leaves_no_header_rule() {
    assert_eq!(
        plain("|  |  |\n|---|---|\n| 1 | 2 |"),
        ["┌───┬───┐", "│ 1 │ 2 │", "└───┴───┘"]
    );
}

#[test]
fn a_table_opening_a_list_item_carries_the_marker_on_its_top_border() {
    assert_eq!(
        plain("- | a | b |\n  |---|---|\n  | 1 | 2 |"),
        [
            "• ┌───┬───┐",
            "  │ a │ b │",
            "  ├───┼───┤",
            "  │ 1 │ 2 │",
            "  └───┴───┘"
        ]
    );
}

#[test]
fn a_table_after_text_in_a_list_item_hangs_under_it() {
    assert_eq!(
        plain("- intro\n\n  | a |\n  |---|\n  | 1 |"),
        ["• intro", "  ┌───┐", "  │ a │", "  ├───┤", "  │ 1 │", "  └───┘"]
    );
}

#[test]
fn a_quoted_table_carries_the_bar_on_every_frame_row() {
    assert_eq!(
        plain("> | a |\n> |---|\n> | 1 |"),
        ["│ ┌───┐", "│ │ a │", "│ ├───┤", "│ │ 1 │", "│ └───┘"]
    );
}

#[test]
fn a_frame_wider_than_the_panel_ends_in_the_ellipsis() {
    let rows = rows(&lines("| alpha | beta |\n|---|---|\n| 1 | 2 |"), 60);

    let clipped: Vec<String> = fitted(&rows, 6, 10).iter().map(|row| joined(row)).collect();

    assert_eq!(clipped[0], "┌────…");
    assert!(clipped.iter().all(|row| cell_count(row) == 6));
}

#[test]
fn empty_cells_and_rows_keep_their_place() {
    let source = "| a | b | c |\n|---|---|---|\n|  | x | y |\n| p |  | r |\n| s | t |  |\n|  |  |  |\n| u | v | w |";

    assert_eq!(
        plain(source),
        [
            "┌───┬───┬───┐",
            "│ a │ b │ c │",
            "├───┼───┼───┤",
            "│   │ x │ y │",
            "│ p │   │ r │",
            "│ s │ t │   │",
            "│   │   │   │",
            "│ u │ v │ w │",
            "└───┴───┴───┘",
        ]
    );
}

// Foundation emits no run for a trailing all-empty row, so upstream drops it; the port keeps that.
#[test]
fn a_trailing_all_empty_row_is_dropped() {
    assert_eq!(
        plain("| a |\n|---|\n| x |\n|  |"),
        ["┌───┐", "│ a │", "├───┤", "│ x │", "└───┘"]
    );
}

#[test]
fn an_image_shows_its_alt_text_and_html_stays_literal() {
    assert_eq!(plain("![diagram](d.png) <b>hi</b>"), ["diagram <b>hi</b>"]);
    assert_eq!(plain("<!-- note -->"), ["<!-- note -->"]);
}

#[test]
fn inline_styles_map_onto_run_styles() {
    assert_eq!(
        styled("**b** *i* ~~s~~ `c` [label](http://x)"),
        [vec![
            run("b", Style::BOLD),
            run(" ", none()),
            run("i", Style::ITALIC),
            run(" ", none()),
            run("s", Style::STRIKETHROUGH),
            run(" ", none()),
            run("c", none()),
            run(" ", none()),
            Run::linked("label", Style::UNDERLINE, "http://x"),
        ]]
    );
}

#[test]
fn a_link_shows_its_underlined_label_alone() {
    assert_eq!(plain("see [the PR](https://example.com/pr/7) now"), ["see the PR now"]);
    assert_eq!(
        styled("**[a b](https://example.com)**")[0],
        [Run::linked(
            "a b",
            Style::BOLD | Style::UNDERLINE,
            "https://example.com"
        )]
    );
    assert!(plain("| h |\n|---|\n| [a](http://x) |").contains(&"│ a │".to_owned()));
    assert_eq!(plain("<https://example.com/a>"), ["https://example.com/a"]);
}

#[test]
fn a_link_the_policy_ignores_is_its_plain_label() {
    for url in [
        "javascript:alert(1)",
        "x-custom://do",
        "relative/path.md",
        "file://other.host/tmp/x",
    ] {
        assert_eq!(styled(&format!("[label]({url})"))[0], [run("label", none())], "{url}");
    }
}

#[test]
fn a_local_file_link_is_clickable() {
    assert_eq!(
        styled("[log](file:///tmp/build.log)")[0][0],
        Run::linked("log", Style::UNDERLINE, "file:///tmp/build.log")
    );
}

#[test]
fn a_link_target_is_always_printable_ascii() {
    assert_eq!(
        link_target("https://example.com/a%1Bb").as_deref(),
        Some("https://example.com/a%1Bb")
    );
    assert_eq!(link_target("https://example.com/a\u{1B}b"), None);
    assert_eq!(
        styled("[x](https://example.com/%D1%84)")[0][0].link.as_deref(),
        Some("https://example.com/%D1%84")
    );
}

#[test]
fn a_link_in_a_heading_keeps_the_heading_style() {
    assert_eq!(
        styled("# [t](http://x) u")[0],
        [
            Run::linked("t", Style::BOLD | Style::UNDERLINE, "http://x"),
            run(" u", Style::BOLD)
        ]
    );
}

#[test]
fn a_wrapped_link_keeps_its_target_on_every_row() {
    let rows = rows(&lines("[ab cd ef](http://x/yz)"), 6);

    assert_eq!(
        rows,
        [
            vec![Run::linked("ab cd", Style::UNDERLINE, "http://x/yz")],
            vec![Run::linked("ef", Style::UNDERLINE, "http://x/yz")],
        ]
    );
}

#[test]
fn a_clipped_link_leaves_the_ellipsis_outside_it() {
    let row = clipped(&[Run::linked("abcdef", Style::UNDERLINE, "http://x")], 4);

    assert_eq!(
        row,
        [Run::linked("abc", Style::UNDERLINE, "http://x"), run("…", none())]
    );
}

#[test]
fn strong_inside_emphasis_carries_both() {
    assert_eq!(
        styled("*a **b***")[0].last(),
        Some(&run("b", Style::BOLD | Style::ITALIC))
    );
}

#[test]
fn decoded_control_characters_are_neutralized() {
    assert_eq!(plain("x &#27;[31m y"), ["x \u{FFFD}[31m y"]);
    assert_eq!(plain("a&#10;b"), ["a\u{FFFD}b"]);
    assert_eq!(plain("a&#9;b"), ["a b"]);
}

#[test]
fn hard_breaks_and_code_newlines_survive_neutralization() {
    assert_eq!(plain("a  \nb\n\n```\nc\nd\n```"), ["a", "b", "", "  c", "  d"]);
}

#[test]
fn wrapping_breaks_at_spaces_across_style_runs() {
    let rows = rows(&lines("aa **bb** cc"), 5);

    assert_eq!(rows.iter().map(|row| joined(row)).collect::<Vec<_>>(), ["aa bb", "cc"]);
    assert_eq!(rows[0], [run("aa ", none()), run("bb", Style::BOLD)]);
}

#[test]
fn a_word_spanning_style_runs_stays_one_word() {
    let rows = rows(&lines("pre**fix**suffix tail"), 12);

    assert_eq!(
        rows.iter().map(|row| joined(row)).collect::<Vec<_>>(),
        ["prefixsuffix", "tail"]
    );
    assert_eq!(rows[0][1], run("fix", Style::BOLD));
}

#[test]
fn literal_spacing_survives_a_wide_row() {
    assert_eq!(laid_out("run `echo \"a  b\"` now", 60), ["run echo \"a  b\" now"]);
    assert_eq!(
        laid_out("<div title=\"a  b\">\n    indented\n</div>", 60),
        ["<div title=\"a  b\">", "    indented", "</div>"]
    );
}

#[test]
fn literal_spacing_survives_wrapping_and_is_dropped_only_at_the_break() {
    assert_eq!(laid_out("`a  b  c`", 4), ["a  b", "c"]);
}

#[test]
fn a_word_longer_than_the_row_is_split() {
    assert_eq!(laid_out("abcdefghij", 4), ["abcd", "efgh", "ij"]);
}

#[test]
fn list_continuations_hang_under_the_item_text() {
    assert_eq!(laid_out("- one two three", 9), ["• one two", "  three"]);
    assert_eq!(laid_out("10. alpha beta", 12), ["10. alpha", "    beta"]);
}

#[test]
fn code_wraps_at_the_width_keeping_its_spaces() {
    assert_eq!(laid_out("```\n    a b\n```", 6), ["      ", "  a b"]);
}

#[test]
fn a_rule_spans_the_widest_other_row_and_a_table_never_wraps() {
    assert_eq!(laid_out("---", 60), ["───"]);
    assert_eq!(laid_out("> ---", 60), ["│ ───"]);
    assert_eq!(
        laid_out("a heading of sorts\n\n---", 60),
        ["a heading of sorts".to_owned(), String::new(), "─".repeat(18)]
    );
    assert_eq!(
        laid_out("a heading of sorts\n\n---", 6).last().map(String::as_str),
        Some("──────")
    );
    assert_eq!(
        laid_out("| long header | other |\n|---|---|\n| x | y |", 6),
        [
            "┌─────────────┬───────┐",
            "│ long header │ other │",
            "├─────────────┼───────┤",
            "│ x           │ y     │",
            "└─────────────┴───────┘",
        ]
    );
}

#[test]
fn a_heading_with_an_inner_strong_span_encodes_as_one_bold_run() {
    let rows = rows(&lines("# **x** y"), 20);

    assert_eq!(sgr(&rows[0]), format!("{ESC}1mx y{ESC}22m"));
}

#[test]
fn sgr_emits_only_the_codes_a_change_needs() {
    let bold = run("a", Style::BOLD);
    let italic = run("b", Style::ITALIC);
    let both = run("c", Style::ITALIC | Style::STRIKETHROUGH);

    assert_eq!(
        sgr(&[bold, italic, both]),
        format!("{ESC}1ma{ESC}22;3mb{ESC}9mc{ESC}23;29m")
    );
    assert_eq!(sgr(&[run("plain", none())]), "plain");
}

#[test]
fn sgr_wraps_a_linked_run_in_a_hyperlink_closed_by_the_row_end() {
    let open = format!("{OSC}8;;http://x{ST}");
    let close = format!("{OSC}8;;{ST}");
    let label = Run::linked("a", Style::UNDERLINE, "http://x");
    let other = Run::linked("c", Style::UNDERLINE, "http://y");

    assert_eq!(
        sgr(&[label.clone(), run(" b", none())]),
        format!("{open}{ESC}4ma{close}{ESC}24m b")
    );
    assert_eq!(
        sgr(std::slice::from_ref(&label)),
        format!("{open}{ESC}4ma{ESC}24m{close}")
    );
    assert_eq!(
        sgr(&[label, other]),
        format!("{open}{ESC}4ma{OSC}8;;http://y{ST}c{ESC}24m{close}")
    );
}

#[test]
fn dropping_bold_reopens_dim_because_22_closes_both() {
    let row = [run("a", Style::BOLD | Style::DIM), run("b", Style::DIM)];

    assert_eq!(sgr(&row), format!("{ESC}1;2ma{ESC}22;2mb{ESC}22m"));
}

#[test]
fn sgr_never_resets_the_text_color() {
    let rows = rows(&lines("# t\n\n**b** *i* ~~s~~\n\n| h |\n|---|\n| c |"), 30);
    let encoded: String = rows.iter().map(|row| sgr(row)).collect();

    assert!(!encoded.contains(&format!("{ESC}0m")));
    assert!(!encoded.contains(&format!("{ESC}m")));
}

#[test]
fn a_too_wide_row_ends_in_an_ellipsis_and_closes_its_style_first() {
    let row = [run("ab", Style::BOLD), run("cdef", none())];

    let clipped_row = clipped(&row, 3);

    assert_eq!(clipped_row, [run("ab", Style::BOLD), run("…", none())]);
    assert_eq!(sgr(&clipped_row), format!("{ESC}1mab{ESC}22m…"));
    assert_eq!(clipped(&row, 6), row);
}

fn numbered_rows() -> Vec<Vec<Run>> {
    (1..=10)
        .map(|index| vec![run(&format!("row {index}"), none())])
        .collect()
}

#[test]
fn overflowing_rows_end_in_a_count_of_every_hidden_row() {
    let fitted = fitted(&numbered_rows(), 20, 5);

    assert_eq!(fitted.len(), 5);
    assert_eq!(fitted[4], [run("… 6 more", Style::DIM)]);
    assert_eq!(
        fitted[..4].iter().map(|row| row[0].text.as_str()).collect::<Vec<_>>(),
        ["row 1", "row 2", "row 3", "row 4"]
    );
}

#[test]
fn the_overflow_marker_is_clipped_like_any_row() {
    let fitted = fitted(&numbered_rows(), 4, 2);

    assert_eq!(
        fitted.iter().map(|row| joined(row)).collect::<Vec<_>>(),
        ["row…", "… 9…"]
    );
}

#[test]
fn tiny_budgets_never_paint_outside_them() {
    let rows = [vec![run("abc", none())], vec![run("def", none())]];

    assert!(fitted(&rows, 0, 5).is_empty());
    assert!(fitted(&rows, 5, 0).is_empty());
    assert_eq!(fitted(&rows, 5, 1), [clipped(&[run("… 2 more", Style::DIM)], 5)]);
    assert_eq!(
        fitted(&rows, 1, 5).iter().map(|row| joined(row)).collect::<Vec<_>>(),
        ["…", "…"]
    );
}

#[test]
fn visible_text_needs_a_non_space_cell() {
    assert!(renders_visible_text("# hi"));
    assert!(renders_visible_text("---"));
    assert!(!renders_visible_text(""));
    assert!(!renders_visible_text("   \n\n"));
    assert!(!renders_visible_text("&nbsp;"));
}
