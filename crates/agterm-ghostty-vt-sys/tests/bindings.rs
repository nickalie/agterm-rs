//! The checked-in bindings carry the value types and `#define` constants that the option families' untyped `void*`
//! values take (key encoder, key events, selection gestures, kitty graphics, device attributes, colors), not only the
//! types a function signature reaches. A regenerated `bindings.rs` that loses one fails to compile here.

use agterm_ghostty_vt_sys as sys;

#[test]
fn option_value_types_are_bound() {
    // GHOSTTY_KEY_ENCODER_OPT_KITTY_FLAGS and _MACOS_OPTION_AS_ALT
    let flags: sys::GhosttyKittyKeyFlags = 0;
    let option_as_alt: sys::GhosttyOptionAsAlt = sys::GHOSTTY_OPTION_AS_ALT_TRUE;
    // GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION and the word-boundary codepoints
    let position = sys::GhosttySurfacePosition { x: 1.0, y: 2.0 };
    let codepoints = sys::GhosttyCodepoints {
        ptr: std::ptr::null(),
        len: 0,
    };
    // kitty graphics image and placement data
    let format: sys::GhosttyKittyImageFormat = sys::GHOSTTY_KITTY_IMAGE_FORMAT_PNG;
    let compression: sys::GhosttyKittyImageCompression = 0;
    let layer: sys::GhosttyKittyPlacementLayer = 0;
    assert_eq!(flags, 0);
    assert_eq!(option_as_alt, 1);
    assert!((position.x - 1.0).abs() < f64::EPSILON && (position.y - 2.0).abs() < f64::EPSILON);
    assert_eq!(codepoints.len, 0);
    assert_eq!((format, compression, layer), (2, 0, 0));
}

#[test]
fn integer_defines_are_bound() {
    // GhosttyMods for ghostty_key_event_set_mods, kitty keyboard flags, device attributes, named colors
    assert_eq!(sys::GHOSTTY_MODS_SHIFT, 1);
    assert_eq!(sys::GHOSTTY_KITTY_KEY_DISAMBIGUATE, 1);
    assert_eq!(sys::GHOSTTY_DA_DEVICE_TYPE_VT220, 1);
    assert_eq!(sys::GHOSTTY_COLOR_NAMED_RED, 1);
}
