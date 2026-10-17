//! The overlay's pure parts: its config and its focus keys (the program panel's rectangle is `split::program_rect`).
//! The overlays on a real page are tested with the deck (`deck/tests/overlay.rs`).

use super::*;

#[test]
fn the_config_paints_only_a_given_background() {
    assert!(config_overlay(None, 1.0).is_empty());
    assert!(config_overlay(Some("#203040"), 1.0).contains("background = #203040"));
    assert!(
        config_overlay(Some("#203040"), 0.5).contains("background-opacity = 0.5"),
        "a translucent window's opacity baked in"
    );
}

#[test]
fn overlay_focus_keys_are_told_from_pane_tokens() {
    assert_eq!(focus_key(3, false), "overlay#3");
    assert_eq!(focus_key(3, true), "overlay-pane#3");
    assert!(is_overlay_key("overlay#3") && is_overlay_key("overlay-pane#7"));
    assert!(!is_overlay_key("6f9619ff-8b86-d011-b42d-00c04fc964ff"));
    assert!(!is_overlay_key("ask:1"));
}
