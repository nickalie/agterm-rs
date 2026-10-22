//! Ported from upstream's `HudTests` (the spec, vocabulary and layout halves; the JSON coding tests belong to the
//! protocol layer).

use super::layout::{
    HORIZONTAL_PADDING, NO_TEXT_COLOR, SPINNER_WIDTH, VERTICAL_PADDING, body_lines, cell_count, height_percent,
    height_points, markdown_body, width_percent, wrap, wrap_columns,
};
use super::*;

fn spec(message: &str) -> HudSpec {
    HudSpec::new(message)
}

fn grid(columns: usize, rows: usize) -> Grid {
    Grid { columns, rows }
}

fn padded(cell_width: f64, cell_height: f64, pane_width: f64, pane_height: f64) -> PaneMetrics {
    PaneMetrics {
        padding_width: 8.0,
        padding_height: 6.0,
        ..PaneMetrics::new(cell_width, cell_height, pane_width, pane_height)
    }
}

fn markdown(message: &str) -> HudSpec {
    HudSpec {
        markdown: true,
        ..spec(message)
    }
}

fn boxed(spec: &HudSpec) -> Grid {
    box_for(spec, MAX_COLUMNS)
}

#[test]
fn holding_the_creation_fields_keeps_the_auto_hide() {
    let live = HudSpec {
        background_color: Some("#101010".to_owned()),
        font_size: Some(18.0),
        ..spec("live")
    };
    let held = HudSpec {
        hide_after: Some(10.0),
        ..spec("deploying")
    }
    .holding_creation_fields(&live);

    assert_eq!(held.hide_after, Some(10.0));
    assert!((held.effective_hide_after() - 10.0).abs() < f64::EPSILON);
    assert_eq!(held.background_color.as_deref(), Some("#101010"));
    assert_eq!(held.font_size, Some(18.0));
    assert_eq!(held.message, "deploying");
    assert!(spec("waiting").effective_hide_after().abs() < f64::EPSILON);
}

#[test]
fn unschedulable_durations_are_rejected_and_schedulable_ones_accepted() {
    for seconds in [
        -0.5,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1e12,
        HudSpec::MAX_HIDE_AFTER + 0.5,
    ] {
        assert!(!HudSpec::is_valid_hide_after(seconds), "{seconds}");
    }
    for seconds in [0.0, 0.25, 10.0, HudSpec::MAX_HIDE_AFTER] {
        assert!(HudSpec::is_valid_hide_after(seconds), "{seconds}");
    }
}

#[test]
fn font_sizes_are_bounded_by_the_range() {
    for points in [6.0, 12.0, 72.0] {
        assert!(HudSpec::is_valid_font_size(points), "{points}");
    }
    for points in [5.9, 72.5, 0.0, -12.0, f64::NAN, f64::INFINITY] {
        assert!(!HudSpec::is_valid_font_size(points), "{points}");
    }
}

#[test]
fn copies_keep_markdown_font_size_sticky_and_frame() {
    let original = HudSpec {
        font_size: Some(20.0),
        sticky: true,
        frame: false,
        ..markdown("# status")
    };

    let held = original.holding_creation_fields(&HudSpec {
        font_size: Some(20.0),
        ..spec("live")
    });
    let resized = original.with_size_percent(Some(40));

    assert!(held.markdown && held.font_size == Some(20.0) && held.sticky && !held.frame);
    assert!(resized.markdown && resized.font_size == Some(20.0) && resized.sticky && !resized.frame);
    assert_eq!(resized.size_percent, Some(40));
}

#[test]
fn short_message_box_is_content_plus_padding() {
    let measured = boxed(&spec("gathering options"));

    assert_eq!(measured.columns, 17 + HORIZONTAL_PADDING * 2);
    assert_eq!(measured.rows, 1 + VERTICAL_PADDING * 2);
}

#[test]
fn empty_message_still_produces_a_one_row_box() {
    let measured = boxed(&spec(""));

    assert_eq!(measured.columns, 1 + HORIZONTAL_PADDING * 2);
    assert_eq!(measured.rows, 1 + VERTICAL_PADDING * 2);
    assert_eq!(rendered_body(&spec(""), measured, 4242), "5 3 0 4242 0.5 - 0\n");
}

#[test]
fn long_single_word_is_broken_at_max_columns() {
    let word = "x".repeat(130);

    let lines = wrap(&word, MAX_COLUMNS);
    let measured = boxed(&spec(&word));

    assert_eq!(lines, ["x".repeat(60), "x".repeat(60), "x".repeat(10)]);
    assert_eq!(measured.columns, MAX_COLUMNS + HORIZONTAL_PADDING * 2);
    assert_eq!(measured.rows, 3 + VERTICAL_PADDING * 2);
}

#[test]
fn word_wrap_keeps_words_whole_up_to_max_columns() {
    let message = vec!["word"; 20].join(" ");

    let lines = wrap(&message, MAX_COLUMNS);

    assert_eq!(lines, [["word"; 12].join(" "), ["word"; 8].join(" ")]);
    assert!(lines.iter().all(|line| cell_count(line) <= MAX_COLUMNS));
}

#[test]
fn detail_follows_message_after_a_single_empty_line() {
    let detailed = HudSpec {
        detail: Some("scanning 4 repositories".to_owned()),
        ..spec("gathering options")
    };

    let measured = boxed(&detailed);

    assert_eq!(
        rendered_body(&detailed, measured, 4242),
        "27 5 0 4242 0.5 - 0\ngathering options\n\nscanning 4 repositories\n"
    );
    assert_eq!(measured.columns, 23 + HORIZONTAL_PADDING * 2);
    assert_eq!(measured.rows, 3 + VERTICAL_PADDING * 2);
}

#[test]
fn empty_detail_adds_no_separator() {
    let blank = HudSpec {
        detail: Some("   ".to_owned()),
        ..spec("working")
    };

    assert_eq!(
        rendered_body(&blank, boxed(&blank), 4242),
        "11 3 0 4242 0.5 - 0\nworking\n"
    );
}

#[test]
fn the_header_carries_the_grid_the_spinner_flag_and_the_owning_pid() {
    let spinning = HudSpec {
        spinner: Some(HudSpinner::Braille),
        ..spec("working")
    };

    assert_eq!(
        rendered_body(&spinning, grid(30, 9), 4242),
        "30 9 1 4242 0.08 - 0 ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏\nworking\n"
    );
}

#[test]
fn a_static_panel_carries_the_slow_interval_and_no_frames() {
    assert_eq!(
        rendered_body(&spec("working"), grid(30, 9), 4242),
        "30 9 0 4242 0.5 - 0\nworking\n"
    );
}

#[test]
fn every_frame_is_one_spaceless_scalar() {
    for style in HudSpinner::ALL {
        assert!(!style.frames().is_empty());
        for frame in style.frames() {
            assert_eq!(cell_count(frame), 1, "{} frame {frame:?}", style.wire_name());
            assert!(
                !frame.contains(' '),
                "{} frame {frame:?} would split",
                style.wire_name()
            );
        }
        assert!(!style.interval().is_empty());
        assert!(style.interval().chars().all(|ch| ch.is_ascii_digit() || ch == '.'));
    }
}

#[test]
fn spinner_names_derive_from_the_cases() {
    assert_eq!(HudSpinner::valid_names_list(), "bar|braille|circle|blocks|dot");
    assert_eq!(HudSpinner::accepted_names_list(), "bar|braille|circle|blocks|dot|none");
    assert_eq!(
        HudSpinner::accepted_names_phrase(),
        "bar, braille, circle, blocks, dot, none"
    );
    assert_eq!(HudSpinner::from_wire("circle"), Some(HudSpinner::Circle));
    assert_eq!(HudSpinner::from_wire(HudSpinner::NONE_NAME), None);
}

#[test]
fn embedded_newlines_become_hard_breaks_with_no_blank_lines() {
    let broken = HudSpec {
        detail: Some("four\nfive".to_owned()),
        ..spec("one\n\ntwo three")
    };

    assert_eq!(body_lines(&broken), ["one", "two three", "", "four", "five"]);
    assert_eq!(boxed(&broken).rows, 5 + VERTICAL_PADDING * 2);
}

#[test]
fn spinner_widens_the_box_without_rewrapping() {
    let plain = boxed(&spec("hi"));
    let spinning = boxed(&HudSpec {
        spinner: Some(HudSpinner::Braille),
        ..spec("hi")
    });

    assert_eq!(spinning.columns, plain.columns + SPINNER_WIDTH);
    assert_eq!(spinning.rows, plain.rows);
}

#[test]
fn each_axis_measures_its_own_need() {
    let pane = PaneMetrics::new(8.0, 18.0, 1200.0, 800.0);

    assert_eq!(width_percent(grid(22, 3), &pane, MAX_SIZE_PERCENT), 15);
    assert_eq!(height_percent(grid(22, 3), &pane), 7);
    assert_eq!(width_percent(grid(22, 20), &pane, MAX_SIZE_PERCENT), 15);
    assert_eq!(height_percent(grid(22, 20), &pane), 45);
}

#[test]
fn a_wide_message_does_not_make_a_tall_panel() {
    let pane = PaneMetrics::new(8.0, 18.0, 700.0, 800.0);
    let wide = HudSpec {
        detail: Some("registry.example.org".to_owned()),
        ..spec("waiting on the package index — this can take a minute on a cold cache")
    };
    let measured = boxed(&wide);

    assert_eq!(width_percent(measured, &pane, MAX_SIZE_PERCENT), 71);
    assert_eq!(height_percent(measured, &pane), 14);
}

#[test]
fn a_caller_override_is_bounded_by_the_same_clamp_the_measurement_takes() {
    assert_eq!(clamp_size_percent(100), MAX_SIZE_PERCENT);
    assert_eq!(clamp_size_percent(1), MIN_SIZE_PERCENT);
    assert_eq!(clamp_size_percent(40), 40);
    assert_eq!(clamp_size_percent(MAX_SIZE_PERCENT), MAX_SIZE_PERCENT);
}

#[test]
fn only_the_width_takes_the_minimum_floor() {
    let pane = PaneMetrics::new(8.0, 18.0, 2000.0, 1000.0);

    assert_eq!(width_percent(grid(6, 3), &pane, MAX_SIZE_PERCENT), MIN_SIZE_PERCENT);
    assert_eq!(height_percent(grid(6, 3), &pane), 6);
}

#[test]
fn a_huge_message_grows_tall_and_is_capped_there() {
    let pane = PaneMetrics::new(8.0, 18.0, 1200.0, 800.0);
    let measured = boxed(&spec(&vec!["word"; 600].join(" ")));

    assert_eq!(width_percent(measured, &pane, MAX_SIZE_PERCENT), 42);
    assert_eq!(height_percent(measured, &pane), MAX_SIZE_PERCENT);
}

#[test]
fn unmeasured_pane_widens_but_does_not_heighten() {
    let pane = PaneMetrics::new(8.0, 18.0, 0.0, 0.0);

    assert_eq!(width_percent(grid(10, 3), &pane, MAX_SIZE_PERCENT), MAX_SIZE_PERCENT);
    assert_eq!(height_percent(grid(10, 3), &pane), MIN_SIZE_PERCENT);
}

#[test]
fn the_header_carries_the_panels_own_grid() {
    let pane = padded(7.8, 15.0, 1000.0, 700.0);
    let message = spec("gathering options…");
    let measured = boxed(&message);
    let width = width_percent(measured, &pane, MAX_SIZE_PERCENT);
    let height = height_percent(measured, &pane);

    let painted = paint_grid(&message, &HudPanelSize::new(width, height), &pane);

    assert_eq!((width, height), (19, 9));
    assert_eq!(painted, grid(22, 3));
    assert_eq!(measured, grid(22, 3));
    assert!(rendered_body(&message, painted, 4242).starts_with("22 3 0 4242 0.5 - 0\n"));
    assert_eq!((painted.rows - body_lines(&message).len()) / 2, 1);
}

#[test]
fn a_caller_override_widens_the_panel_without_heightening_it() {
    let pane = padded(7.8, 15.0, 1000.0, 700.0);
    let sized = HudSpec {
        size_percent: Some(MAX_SIZE_PERCENT),
        ..spec("ok")
    };
    let size = panel_size(&sized, &pane);

    let painted = paint_grid(&sized, &size, &pane);

    assert_eq!(size.width_percent, MAX_SIZE_PERCENT);
    assert_eq!(size.height_percent, 9);
    assert_eq!(painted, grid(100, 3));
    assert_eq!(boxed(&sized), grid(6, 3));
}

#[test]
fn paint_grid_falls_back_to_the_box_without_a_measured_pane() {
    let working = spec("working");
    let unmeasured = PaneMetrics::new(8.0, 18.0, 0.0, 0.0);
    let no_cell = PaneMetrics::new(0.0, 0.0, 1000.0, 700.0);

    assert_eq!(panel_grid(&HudPanelSize::new(40, 10), &unmeasured), None);
    assert_eq!(panel_grid(&HudPanelSize::new(40, 10), &no_cell), None);
    assert_eq!(
        paint_grid(&working, &HudPanelSize::new(80, 10), &unmeasured),
        boxed(&working)
    );
    let tiny = padded(8.0, 18.0, 30.0, 30.0);
    assert_eq!(panel_grid(&HudPanelSize::new(40, 40), &tiny), None);
}

#[test]
fn width_is_counted_in_the_unit_the_helper_counts() {
    let decomposed = "cafe\u{0301} au lait";

    let lines = body_lines(&spec(decomposed));

    assert_eq!(lines.len(), 1);
    assert_eq!(cell_count(&lines[0]), 12);
    assert_eq!(boxed(&spec(decomposed)).columns, 12 + HORIZONTAL_PADDING * 2);
    assert_eq!(text_length(decomposed), 12);
}

#[test]
fn a_zwj_sequence_is_measured_in_code_points_not_clusters() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}";

    assert_eq!(cell_count(family), 5);
    assert_eq!(boxed(&spec(family)).columns, 5 + HORIZONTAL_PADDING * 2);
}

#[test]
fn a_markdown_body_carries_its_block_width_and_encoded_rows() {
    assert_eq!(
        rendered_body(&markdown("**hi** there"), grid(20, 5), 4242),
        "20 5 0 4242 0.5 - 8\n\u{1B}[1mhi\u{1B}[22m there\n"
    );
}

#[test]
fn a_markdown_link_adds_no_width_beyond_its_visible_text() {
    let body = markdown_body(&markdown("[ab](http://x)"), grid(40, 5));

    assert_eq!(body.block_width, 2);
    assert_eq!(
        body.lines,
        ["\u{1B}]8;;http://x\u{1B}\\\u{1B}[4mab\u{1B}[24m\u{1B}]8;;\u{1B}\\"]
    );
}

#[test]
fn a_markdown_message_is_measured_at_the_clamped_width_it_is_painted_at() {
    for percent in [100, 5] {
        let pane = PaneMetrics::new(8.0, 18.0, 576.0, 2000.0);
        let sized = HudSpec {
            size_percent: Some(percent),
            ..markdown(&vec!["word"; 24].join(" "))
        };

        let measured = panel_size(&sized, &pane);
        let taken = HudPanelSize::new(clamp_size_percent(percent), measured.height_percent);
        let painted = paint_grid(&sized, &taken, &pane);
        let body = markdown_body(&sized, painted);

        assert!(!body.lines.concat().contains("more"), "{percent}");
        assert!(painted.rows - body.lines.len() - VERTICAL_PADDING * 2 <= 1, "{percent}");
    }
}

#[test]
fn a_spinning_markdown_body_indents_every_row_after_the_first_by_the_gutter() {
    let spinning = HudSpec {
        spinner: Some(HudSpinner::Bar),
        ..markdown("- a\n- bb")
    };

    let body = markdown_body(&spinning, grid(20, 6));

    assert_eq!(body.lines, ["• a", "  • bb"]);
    assert_eq!(body.block_width, 4 + SPINNER_WIDTH);
}

#[test]
fn a_markdown_body_wraps_at_the_grid_less_its_padding_and_counts_what_does_not_fit() {
    let message: Vec<String> = (1..=10).map(|index| format!("- item {index}")).collect();

    let body = markdown_body(&markdown(&message.join("\n")), grid(9, 5));

    assert_eq!(body.lines, ["• ite", "  m 1", "\u{1B}[2m… 19\u{1B}[22m…"]);
    assert_eq!(body.block_width, 5);
}

#[test]
fn a_markdown_table_wider_than_the_panel_is_still_clipped() {
    let body = markdown_body(&markdown("| abcdef |\n|---|\n| x |"), grid(10, 9));

    assert_eq!(body.lines[0], "┌────…");
}

#[test]
fn a_markdown_message_is_measured_and_wrapped_at_the_width_the_pane_allows() {
    let pane = PaneMetrics::new(8.0, 18.0, 400.0, 800.0);
    let linked = markdown("aaaa bbbb cccc dddd eeee ffff gggg [the link](http://x) hhhh iiii jjjj");

    let size = panel_size(&linked, &pane);
    let painted = paint_grid(&linked, &size, &pane);
    let body = markdown_body(&linked, painted);

    assert_eq!(painted.columns, 38);
    assert_eq!(
        body.lines,
        [
            "aaaa bbbb cccc dddd eeee ffff gggg",
            "\u{1B}]8;;http://x\u{1B}\\\u{1B}[4mthe link\u{1B}]8;;\u{1B}\\\u{1B}[24m hhhh iiii jjjj",
        ]
    );
    assert_eq!(painted.rows, body.lines.len() + VERTICAL_PADDING * 2);
}

#[test]
fn a_one_row_spinning_markdown_body_shows_only_the_marker() {
    let spinning = HudSpec {
        spinner: Some(HudSpinner::Bar),
        ..markdown("a\n\nb")
    };

    let body = markdown_body(&spinning, grid(30, 3));

    assert_eq!(body.lines, ["\u{1B}[2m… 3 more\u{1B}[22m"]);
    assert_eq!(body.block_width, 8 + SPINNER_WIDTH);
}

#[test]
fn an_empty_markdown_budget_still_writes_a_markdown_block_width() {
    let body = markdown_body(&markdown("a"), grid(4, 2));

    assert!(body.lines.is_empty());
    assert_eq!(body.block_width, 1);
}

#[test]
fn a_markdown_detail_follows_a_blank_row_dimmed() {
    let detailed = HudSpec {
        detail: Some("d".to_owned()),
        ..markdown("x")
    };

    assert_eq!(
        markdown_body(&detailed, grid(20, 10)).lines,
        ["x", "", "\u{1B}[2md\u{1B}[22m"]
    );
}

#[test]
fn the_markdown_box_measures_the_rendered_rows() {
    let measured = boxed(&markdown("# T\n\n- a\n- b"));

    assert_eq!(measured.rows, 4 + VERTICAL_PADDING * 2);
    assert_eq!(measured.columns, 3 + HORIZONTAL_PADDING * 2);
}

#[test]
fn position_names_derive_from_the_cases() {
    assert_eq!(
        HudPosition::valid_names_list(),
        "top-left|top-center|top-right|center-left|center|center-right|bottom-left|bottom-center|bottom-right"
    );
    assert_eq!(
        HudPosition::valid_names_phrase(),
        "top-left, top-center, top-right, center-left, center, center-right, bottom-left, bottom-center, bottom-right"
    );
    assert_eq!(
        HudPosition::accepted_names_list(),
        HudPosition::valid_names_list() + "|top|bottom"
    );
    assert_eq!(
        HudPosition::accepted_names_phrase(),
        HudPosition::valid_names_phrase() + ", top, bottom"
    );
}

#[test]
fn bare_aliases_normalize_to_the_middle_column() {
    assert_eq!(HudPosition::parse("top"), Some(HudPosition::TopCenter));
    assert_eq!(HudPosition::parse("bottom"), Some(HudPosition::BottomCenter));
    for raw in ["middle", "top-middle", "TOP", "", "left"] {
        assert_eq!(HudPosition::parse(raw), None, "{raw}");
    }
    for position in HudPosition::ALL {
        assert_eq!(HudPosition::parse(position.wire_name()), Some(position));
    }
}

#[test]
fn bands_split_the_anchors_into_three_rows_and_three_columns() {
    let leading: Vec<HudPosition> = HudPosition::ALL
        .into_iter()
        .filter(|position| position.vertical_band() == Band::Leading)
        .collect();
    let trailing: Vec<HudPosition> = HudPosition::ALL
        .into_iter()
        .filter(|position| position.horizontal_band() == Band::Trailing)
        .collect();

    assert_eq!(
        leading,
        [HudPosition::TopLeft, HudPosition::TopCenter, HudPosition::TopRight]
    );
    assert_eq!(
        trailing,
        [
            HudPosition::TopRight,
            HudPosition::CenterRight,
            HudPosition::BottomRight
        ]
    );
    assert_eq!(HudPosition::Center.vertical_band(), Band::Middle);
    assert_eq!(HudPosition::Center.horizontal_band(), Band::Middle);
}

#[test]
fn edge_margin_leaves_room_for_the_largest_panel_on_both_axes() {
    assert!(HudPosition::EDGE_MARGIN_PERCENT * 2 + clamp_size_percent(100) <= 100);
}

#[test]
fn text_color_encodes_as_truecolor_sgr_parameters() {
    for (hex, expected) in [
        ("#000000", "38;2;0;0;0"),
        ("#ffffff", "38;2;255;255;255"),
        ("#7ec07e", "38;2;126;192;126"),
        ("e0e0e0", "38;2;224;224;224"),
    ] {
        assert_eq!(foreground_sgr(Some(hex)), expected);
    }
}

#[test]
fn absent_or_malformed_text_color_falls_back_to_the_terminal_foreground() {
    assert_eq!(foreground_sgr(None), NO_TEXT_COLOR);
    for hex in ["", "#12345", "#gggggg", "rebeccapurple"] {
        assert_eq!(foreground_sgr(Some(hex)), NO_TEXT_COLOR, "{hex}");
    }
}

#[test]
fn the_header_carries_the_text_color_before_the_frames() {
    let colored = HudSpec {
        spinner: Some(HudSpinner::Circle),
        text_color: Some("#7ec07e".to_owned()),
        ..spec("working")
    };

    let body = rendered_body(&colored, grid(30, 9), 4242);

    assert_eq!(body.lines().next(), Some("30 9 1 4242 0.12 38;2;126;192;126 0 ◐ ◓ ◑ ◒"));
}

#[test]
fn a_markdown_message_with_a_caller_width_wraps_at_that_width() {
    let pane = PaneMetrics::new(8.0, 18.0, 1600.0, 800.0);
    let line = vec!["word"; 20].join(" ");
    let sized = HudSpec {
        size_percent: Some(80),
        ..markdown(&line)
    };
    let measured = markdown(&line);

    let size = panel_size(&sized, &pane);
    let body = markdown_body(&sized, paint_grid(&sized, &size, &pane));

    assert_eq!(body.lines, [line]);
    assert_eq!(
        box_for(&measured, wrap_columns(&measured, &pane)).rows,
        2 + VERTICAL_PADDING * 2
    );
}

#[test]
fn a_sticky_panel_may_span_the_pane_unless_it_is_centered() {
    for position in HudPosition::ALL {
        let loose = HudSpec {
            position,
            ..spec("caption")
        };
        let sticky = HudSpec {
            sticky: true,
            ..loose.clone()
        };
        let widest = if position == HudPosition::Center {
            MAX_SIZE_PERCENT
        } else {
            100
        };

        assert_eq!(clamp_size_percent_for(100, &sticky), widest, "{}", position.wire_name());
        assert_eq!(clamp_size_percent_for(100, &loose), MAX_SIZE_PERCENT);
        assert_eq!(clamp_size_percent_for(1, &sticky), MIN_SIZE_PERCENT);
    }
}

#[test]
fn a_measured_sticky_panel_takes_the_width_its_text_needs_up_to_the_pane() {
    let pane = PaneMetrics::new(8.0, 18.0, 400.0, 800.0);
    let loose = HudSpec {
        position: HudPosition::TopCenter,
        ..spec(&"x".repeat(58))
    };
    let sticky = HudSpec {
        sticky: true,
        ..loose.clone()
    };

    assert_eq!(panel_size(&sticky, &pane).width_percent, 100);
    assert_eq!(panel_size(&loose, &pane).width_percent, MAX_SIZE_PERCENT);
}

#[test]
fn a_frameless_panel_is_as_tall_as_its_text() {
    let pane = padded(8.0, 18.0, 1200.0, 800.0);
    let frameless = HudSpec {
        frame: false,
        ..markdown("- a\n- b")
    };

    let size = panel_size(&frameless, &pane);
    let painted = paint_grid(&frameless, &size, &pane);

    assert_eq!(boxed(&frameless).rows, 2);
    assert_eq!(
        boxed(&HudSpec {
            frame: false,
            ..spec("one line")
        })
        .rows,
        1
    );
    assert_eq!(size.height_points, Some(48.0));
    assert_eq!(painted.rows, 2);
    assert_eq!(markdown_body(&frameless, painted).lines, ["• a", "• b"]);
}

#[test]
fn the_height_in_points_does_not_follow_the_pane() {
    for pane_height in [400.0, 800.0, 1600.0] {
        let pane = padded(8.0, 18.0, 1200.0, pane_height);
        let message = spec("gathering options");

        let size = panel_size(&message, &pane);

        assert_eq!(size.height_points, Some(66.0), "{pane_height}");
        assert_eq!(paint_grid(&message, &size, &pane).rows, 3, "{pane_height}");
    }
}

#[test]
fn a_fractional_cell_still_yields_the_whole_rows_the_points_were_built_from() {
    for cell_height in [15.4, 17.3, 19.7] {
        let pane = padded(7.8, cell_height, 1200.0, 900.0);
        let frameless = HudSpec {
            frame: false,
            ..markdown("- a\n- b\n- c")
        };

        let size = panel_size(&frameless, &pane);

        assert_eq!(paint_grid(&frameless, &size, &pane).rows, 3, "{cell_height}");
    }
}

#[test]
fn content_past_the_height_cap_agrees_on_points_grid_body_and_percent() {
    let pane = padded(8.0, 18.0, 1200.0, 800.0);
    let items: Vec<String> = (1..=80).map(|index| format!("- item {index}")).collect();
    let long = markdown(&items.join("\n"));

    let size = panel_size(&long, &pane);
    let painted = paint_grid(&long, &size, &pane);
    let body = markdown_body(&long, painted);

    assert_eq!(size.height_points, Some(640.0));
    assert_eq!(size.height_percent, MAX_SIZE_PERCENT);
    assert_eq!(painted.rows, 34);
    assert_eq!(body.lines.len(), painted.rows - VERTICAL_PADDING * 2);
    assert!(body.lines.last().is_some_and(|line| line.contains("more")));
}

#[test]
fn an_unmeasured_pane_has_no_height_in_points() {
    let pane = PaneMetrics::new(8.0, 18.0, 0.0, 0.0);

    assert_eq!(panel_size(&spec("working"), &pane).height_points, None);
    assert_eq!(height_points(grid(10, 3), &pane), None);
}

#[test]
fn pane_frames_merge_only_what_the_other_has() {
    let frame = |x: f64| HudPaneFrame {
        x,
        y: 0.0,
        width: 10.0,
        height: 10.0,
    };
    let mut frames = HudPaneFrames {
        left: Some(frame(1.0)),
        right: Some(frame(2.0)),
        detail: None,
    };

    frames.merge(&HudPaneFrames {
        right: Some(frame(3.0)),
        detail: Some(frame(4.0)),
        ..HudPaneFrames::default()
    });

    assert_eq!(frames.pane(false), Some(frame(1.0)));
    assert_eq!(frames.pane(true), Some(frame(3.0)));
    assert_eq!(frames.detail, Some(frame(4.0)));
}
