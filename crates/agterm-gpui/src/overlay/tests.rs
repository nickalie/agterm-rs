//! The overlay's pure parts: the program panel's rectangle, its config, its focus keys. The overlays on a real page
//! are tested with the deck (`deck/tests/overlay.rs`).

use super::*;

#[test]
fn a_covering_program_takes_the_page_and_a_floating_one_its_share_centered() {
    let full = crate::split::Rect::full(800, 600);
    assert_eq!(program_rect(ProgramPlacement { fraction: None }, 800, 600), full);
    assert_eq!(
        program_rect(ProgramPlacement { fraction: Some(0.5) }, 800, 600),
        crate::split::Rect {
            x: 200,
            y: 150,
            width: 400,
            height: 300
        }
    );
    assert_eq!(
        program_rect(ProgramPlacement { fraction: Some(1.5) }, 800, 600),
        full,
        "a share past the page is the page"
    );
}

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
