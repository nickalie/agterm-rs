use super::{DATA, entries, get};
use crate::terminal::color::Rgb;

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

#[test]
fn map() {
    let cases: &[(&[u8], Option<Rgb>)] = &[
        (b"nosuchcolor", None),
        (b"white", Some(rgb(255, 255, 255))),
        (b"medium spring green", Some(rgb(0, 250, 154))),
        (b"ForestGreen", Some(rgb(34, 139, 34))),
        (b"FoReStGReen", Some(rgb(34, 139, 34))),
        (b"black", Some(rgb(0, 0, 0))),
        (b"red", Some(rgb(255, 0, 0))),
        (b"green", Some(rgb(0, 255, 0))),
        (b"blue", Some(rgb(0, 0, 255))),
        (b"lawngreen", Some(rgb(124, 252, 0))),
        (b"mediumspringgreen", Some(rgb(0, 250, 154))),
        (b"forestgreen", Some(rgb(34, 139, 34))),
    ];
    for &(name, want) in cases {
        assert_eq!(want, get(name), "{name:?}");
    }
}

#[test]
fn entries_in_file_order() {
    let ents = entries();
    assert!(ents.len() > 700);
    for entry in ents {
        assert_eq!(Some(entry.color), get(entry.name.as_bytes()));
    }
    assert_eq!("snow", ents[0].name);
    assert_eq!(rgb(255, 250, 250), ents[0].color);
}

/// Every line of `rgb.txt` is an entry: the parse refuses none (Zig's comptime parse would fail the build).
#[test]
fn every_line_parses() {
    assert_eq!(DATA.lines().count(), entries().len());
}
