//! Ported from upstream's `CommandFailureTests`.

use crate::hud::{HudSpec, text_length};
use crate::keymap::failure::{TAIL_LIMIT, detail, message};

#[test]
fn the_tail_limit_is_sixteen_kibibytes() {
    assert_eq!(TAIL_LIMIT, 16 * 1024);
}

#[test]
fn detail_takes_the_last_nonblank_line() {
    assert_eq!(
        detail(b"first failure\nsecond failure\n\n   \n").as_deref(),
        Some("second failure")
    );
}

#[test]
fn detail_reads_a_last_line_with_no_newline() {
    assert_eq!(detail(b"only line").as_deref(), Some("only line"));
}

#[test]
fn detail_is_none_without_usable_output() {
    assert_eq!(detail(b""), None);
    assert_eq!(detail(b"\n  \n"), None);
}

#[test]
fn detail_strips_escape_sequences_that_would_paint_outside_the_panel() {
    assert_eq!(
        detail("\u{1b}[31mred error\u{1b}[0m".as_bytes()).as_deref(),
        Some("red error")
    );
}

#[test]
fn detail_skips_a_last_line_that_is_nothing_but_a_colour_reset() {
    assert_eq!(
        detail("real error\n\u{1b}[0m\n".as_bytes()).as_deref(),
        Some("real error")
    );
}

#[test]
fn detail_skips_a_last_line_that_is_nothing_but_control_characters() {
    assert_eq!(detail("real error\n\u{07}\n".as_bytes()).as_deref(), Some("real error"));
}

#[test]
fn detail_drops_an_osc_sequence_whole_rather_than_showing_its_payload() {
    assert_eq!(
        detail("\u{1b}]0;a title\u{07}plain error".as_bytes()).as_deref(),
        Some("plain error")
    );
    assert_eq!(
        detail("\u{1b}]0;a title\u{1b}\\plain error".as_bytes()).as_deref(),
        Some("plain error")
    );
}

#[test]
fn detail_decodes_a_tail_cut_mid_scalar() {
    let bytes = "ошибка".as_bytes();
    let detail = detail(&bytes[1..]).unwrap_or_default();
    assert!(detail.contains("шибка"), "{detail}");
}

#[test]
fn detail_is_capped_to_what_a_hud_accepts() {
    let detail = detail("e".repeat(400).as_bytes()).unwrap_or_default();
    assert!(!detail.is_empty());
    assert!(text_length(&detail) <= HudSpec::MAX_TEXT_LENGTH);
}

#[test]
fn detail_caps_a_multi_scalar_grapheme_by_scalars_not_characters() {
    // a ZWJ family stays five scalars after NFC, so 100 of them run far past the cap.
    let family = "👨‍👩‍👧".repeat(100);
    let detail = detail(family.as_bytes()).unwrap_or_default();
    assert!(!detail.is_empty());
    assert!(text_length(&detail) <= HudSpec::MAX_TEXT_LENGTH);
    assert!(detail.chars().count() < family.chars().count());
}

#[test]
fn message_names_the_command_and_the_reason() {
    assert_eq!(message("Agent Reset", "exit 69"), "Agent Reset: exit 69");
}

#[test]
fn message_strips_control_characters_from_the_name() {
    assert_eq!(message("Agent\u{07}Reset", "exit 1"), "AgentReset: exit 1");
}

#[test]
fn message_truncates_the_name_and_keeps_the_reason() {
    let message = message(&"n".repeat(400), "exit 1");
    assert!(text_length(&message) <= HudSpec::MAX_TEXT_LENGTH);
    assert!(
        message.ends_with(": exit 1"),
        "the reason is the half that says what happened"
    );
    assert!(message.starts_with("nnn"));
}

#[test]
fn message_keeps_the_reason_alone_when_it_fills_the_cap_by_itself() {
    let message = message("probe", &"r".repeat(400));
    assert!(text_length(&message) <= HudSpec::MAX_TEXT_LENGTH);
    assert!(message.starts_with("rrr"));
}
