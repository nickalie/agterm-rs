//! The overlay's pure parts: its config and its focus keys (the program panel's rectangle is `split::program_rect`).
//! The overlays on a real page are tested with the deck (`deck/tests/overlay.rs`).

use super::*;

#[test]
fn the_config_paints_only_a_given_background() {
    assert!(config_overlay(None).is_empty());
    assert!(config_overlay(Some("#203040")).contains("background = #203040"));
}

#[test]
fn overlay_focus_keys_are_told_from_pane_tokens() {
    assert_eq!(focus_key(3, false), "overlay#3");
    assert_eq!(focus_key(3, true), "overlay-pane#3");
    assert!(is_overlay_key("overlay#3") && is_overlay_key("overlay-pane#7"));
    assert!(!is_overlay_key("6f9619ff-8b86-d011-b42d-00c04fc964ff"));
    assert!(!is_overlay_key("ask:1"));
}
