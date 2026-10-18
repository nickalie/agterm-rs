use super::{Event, Shape, event_sends_motion};

#[test]
fn cursor_shape_from_string() {
    assert_eq!(Some(Shape::Default), Shape::from_string(b"default"));
}

#[test]
fn every_w3c_and_xcursor_name_maps() {
    assert_eq!(Some(Shape::ZoomOut), Shape::from_string(b"zoom-out"));
    assert_eq!(Some(Shape::NwseResize), Shape::from_string(b"nwse-resize"));
    assert_eq!(Some(Shape::Default), Shape::from_string(b"left_ptr"));
    assert_eq!(Some(Shape::AllScroll), Shape::from_string(b"fleur"));
    assert_eq!(Some(Shape::WResize), Shape::from_string(b"left_side"));
    assert_eq!(None, Shape::from_string(b"nonexistent"));
    assert_eq!(None, Shape::from_string(b"Default"));
    assert_eq!(None, Shape::from_string(b""));
}

#[test]
fn motion_events() {
    assert!(event_sends_motion(Event::Button));
    assert!(event_sends_motion(Event::Any));
    assert!(!event_sends_motion(Event::None));
    assert!(!event_sends_motion(Event::X10));
    assert!(!event_sends_motion(Event::Normal));
}
