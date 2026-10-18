use super::{
    DEFAULT, Dynamic, DynamicPalette, DynamicRGB, InvalidFormat, Name, NoDefaultValue, PaletteMask, Rgb, Special,
};
use crate::mem::alloc::OutOfMemory;
use crate::mem::testing::with_failing;

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

#[test]
fn rgb_encode() {
    let color = rgb(0x01, 0x23, 0xff);
    let mut buf = Vec::new();
    color.encode_rgb8(&mut buf).unwrap();
    assert_eq!(b"rgb:01/23/ff", &buf[..]);
    buf.clear();
    color.encode_rgb16(&mut buf).unwrap();
    assert_eq!(b"rgb:0101/2323/ffff", &buf[..]);
}

#[test]
fn rgb_parse_bare_hex_and_trim() {
    assert_eq!(Ok(rgb(10, 11, 12)), Rgb::parse(b"0A0B0C"));
    assert_eq!(Ok(rgb(255, 255, 255)), Rgb::parse(b"FFFFFF"));
    assert_eq!(Ok(rgb(255, 255, 255)), Rgb::parse(b"FFF"));
    assert_eq!(Ok(rgb(51, 68, 85)), Rgb::parse(b"#345"));
    assert_eq!(Ok(rgb(170, 187, 204)), Rgb::parse(b" #AABBCC   "));
}

#[test]
fn rgb_layout() {
    assert_eq!(3, core::mem::size_of::<Rgb>());
}

#[test]
fn osc4() {
    assert_eq!(256, Special::Bold.osc4());
    assert_eq!(257, Special::Underline.osc4());
    assert_eq!(258, Special::Blink.osc4());
    assert_eq!(259, Special::Reverse.osc4());
    assert_eq!(260, Special::Italic.osc4());
}

#[test]
fn special_from_int() {
    for v in 0..5 {
        assert_eq!(Some(v), Special::from_int(v).map(Special::int));
    }
    assert_eq!(None, Special::from_int(5));
}

#[test]
fn next() {
    let order = [
        Dynamic::Foreground,
        Dynamic::Background,
        Dynamic::Cursor,
        Dynamic::PointerForeground,
        Dynamic::PointerBackground,
        Dynamic::TektronixForeground,
        Dynamic::TektronixBackground,
        Dynamic::HighlightBackground,
        Dynamic::TektronixCursor,
        Dynamic::HighlightForeground,
    ];
    for pair in order.windows(2) {
        assert_eq!(Some(pair[1]), pair[0].next());
    }
    assert_eq!(None, Dynamic::HighlightForeground.next());
}

/// `termio/stream_handler.zig`'s "dynamic color {s} not implemented" logs print `@tagName(dynamic)`, the Zig field
/// name, not the Rust variant name.
#[test]
fn dynamic_tag_names_are_the_zig_field_names() {
    let rows = [
        (Dynamic::Foreground, "foreground"),
        (Dynamic::Background, "background"),
        (Dynamic::Cursor, "cursor"),
        (Dynamic::PointerForeground, "pointer_foreground"),
        (Dynamic::PointerBackground, "pointer_background"),
        (Dynamic::TektronixForeground, "tektronix_foreground"),
        (Dynamic::TektronixBackground, "tektronix_background"),
        (Dynamic::HighlightBackground, "highlight_background"),
        (Dynamic::TektronixCursor, "tektronix_cursor"),
        (Dynamic::HighlightForeground, "highlight_foreground"),
    ];
    for (dynamic, name) in rows {
        assert_eq!(name, dynamic.tag_name());
        assert_eq!(Some(dynamic), Dynamic::from_int(dynamic.int()));
    }
    assert_eq!(None, Dynamic::from_int(20));
}

#[test]
fn palette_default() {
    // Safety check
    for i in 0..16_u8 {
        assert_eq!(Name::from_int(i).default(), Ok(DEFAULT[usize::from(i)]));
        assert_eq!(Name::from_int(i).int(), i);
    }
    assert_eq!(Name::from_int(16), Name::Other(16));
    assert_eq!(Name::Other(16).default(), Err(NoDefaultValue));
    assert_eq!(Name::BrightWhite.int(), 15);
}

#[test]
fn palette_default_cube_and_ramp() {
    assert_eq!(rgb(0, 0, 0), DEFAULT[16]);
    assert_eq!(rgb(0, 0, 95), DEFAULT[17]);
    assert_eq!(rgb(255, 255, 255), DEFAULT[231]);
    assert_eq!(rgb(8, 8, 8), DEFAULT[232]);
    assert_eq!(rgb(238, 238, 238), DEFAULT[255]);
}

/// `RGB.parse` cases that produce a color.
const PARSED: &[(&[u8], Rgb)] = &[
    (b"rgbi:1.0/0/0", rgb(255, 0, 0)),
    (b"rgb:7f/a0a0/0", rgb(127, 160, 0)),
    (b"rgb:f/ff/fff", rgb(255, 255, 255)),
    (b"#ffffff", rgb(255, 255, 255)),
    (b"#fff", rgb(255, 255, 255)),
    (b"#fffffffff", rgb(255, 255, 255)),
    (b"#ffffffffffff", rgb(255, 255, 255)),
    (b"#ff0010", rgb(255, 0, 16)),
    (b"black", rgb(0, 0, 0)),
    (b"red", rgb(255, 0, 0)),
    (b"green", rgb(0, 255, 0)),
    (b"blue", rgb(0, 0, 255)),
    (b"white", rgb(255, 255, 255)),
    (b"LawnGreen", rgb(124, 252, 0)),
    (b"medium spring green", rgb(0, 250, 154)),
    (b" Forest Green ", rgb(34, 139, 34)),
    (b"\tForestGreen\t", rgb(34, 139, 34)),
];

/// `RGB.parse` cases that are `error.InvalidFormat`.
const INVALID: &[&[u8]] = &[
    b"",
    b"  ",
    b"rgb;",
    b"rgb:",
    b":a/a/a",
    b"a/a/a",
    b"rgb:a/a/a/",
    b"rgb:00000///",
    b"rgb:000/",
    b"rgbi:a/a/a",
    b"rgb:0.5/0.0/1.0",
    b"rgb:not/hex/zz",
    b"#",
    b"#ff",
    b"#ffff",
    b"#fffff",
    b"#gggggg",
    b"#12345",
    b"12345",
    b"nosuchcolor",
];

#[test]
fn rgb_parse() {
    for &(spec, want) in PARSED {
        assert_eq!(Ok(want), Rgb::parse(spec), "{spec:?}");
    }
    for &spec in INVALID {
        assert_eq!(Err(InvalidFormat), Rgb::parse(spec), "{spec:?}");
    }
}

/// Intensity semantics (`946422dbc`): `fraction.parse` rejects everything `parseFloat` accepted beyond simple
/// decimals (`NaN`, `inf`, exponents) and out-of-range values. `lib.parseInt` (`73768913b`) accepts no sign prefix on
/// an unsigned type, nor `_` as a digit separator, interior or not.
#[test]
fn rgb_parse_intensity_and_hex_zig_edge_semantics() {
    let invalid: &[&[u8]] = &[
        b"rgbi:NaN/0/0",
        b"rgbi:nan/0/0",
        b"rgbi:nan/1.0/0",
        b"rgbi:inf/0/0",
        b"rgbi:-inf/0/0",
        b"rgbi:1e-1/0/0",
        b"rgbi:1.5/0/0",
        b"rgbi:-0.5/0/0",
        b"rgb:+f/0/0",
        b"rgb:+0/0/0",
        b"rgb:+ff/0/0",
        b"rgb:-f/0/0",
        b"#+f0010",
        b"rgb:f_f/0/0",
        b"rgb:f_ff/0/0",
        b"rgb:_f/0/0",
        b"rgb:f_/0/0",
        b"#f_f000000",
    ];
    for &spec in invalid {
        assert_eq!(Err(InvalidFormat), Rgb::parse(spec), "{spec:?}");
    }
    // ...while simple decimals (including the sign forms fraction.parse allows) still work.
    assert_eq!(Ok(rgb(0, 255, 0)), Rgb::parse(b"rgbi:0/1.0/0"));
    assert_eq!(Ok(rgb(0, 0, 0)), Rgb::parse(b"rgbi:-0/0/0"));
}

/// The parser slices bytes, as Ghostty's does: a multi-byte scalar or invalid UTF-8 inside a spec is bad hex or an
/// unknown name, never a panic, from every arm (rustty's fuzz findings `osc4-rgb-parse-char-boundary` and
/// `osc21-kitty-color-lossy-char-boundary`).
#[test]
fn parse_byte_slices_never_panics_on_a_non_char_boundary() {
    let invalid: &[&[u8]] = &[
        "aébcd".as_bytes(),
        b"a\xc3\xa9bcd",
        "aé".as_bytes(),
        "€".as_bytes(),
        "#aé".as_bytes(),
        "#aébcd".as_bytes(),
        "#aébcdefg".as_bytes(),
        "#aébcdefghij".as_bytes(),
        "rgb:é/0/0".as_bytes(),
        "rgbi:é/0/0".as_bytes(),
        "rgb:0/0/é".as_bytes(),
        "rgbé:0/0/0".as_bytes(),
        "rgé:a/a/a".as_bytes(),
        "  aébcd  ".as_bytes(),
        b"U\x8evv",
        b"\xff\xfe\xfd",
        b"\x80\x80\x80\x80\x80\x80",
        b"#\xff\xff\xff\xff\xff\xff",
        b"rgb:\xff/\xff/\xff",
        b"rgbi:\xff/\xff/\xff",
        b"re\xffd",
    ];
    for &spec in invalid {
        assert_eq!(Err(InvalidFormat), Rgb::parse(spec), "{spec:?}");
    }
    // The ASCII forms that share these arms still parse.
    assert_eq!(Ok(rgb(255, 0, 0)), Rgb::parse(b"f00"));
    assert_eq!(Ok(rgb(255, 0, 0)), Rgb::parse(b"ff0000"));
    assert_eq!(Ok(rgb(255, 0, 0)), Rgb::parse(b"red"));
}

#[test]
fn dynamic_palette_init() {
    let p = DynamicPalette::default();
    assert_eq!(DEFAULT, p.current);
    assert_eq!(DEFAULT, *p.original());
    assert!(p.original_is_shared());
    assert_eq!(0, p.mask.count());
}

#[test]
fn dynamic_palette_set() {
    let mut p = DynamicPalette::default();
    let new_color = rgb(255, 0, 0);

    p.set(0, new_color);
    assert_eq!(new_color, p.current[0]);
    assert!(p.mask.is_set(0));
    assert_eq!(1, p.mask.count());

    assert_eq!(DEFAULT[0], p.original()[0]);
}

#[test]
fn dynamic_palette_reset() {
    let mut p = DynamicPalette::default();
    p.set(0, rgb(255, 0, 0));
    assert!(p.mask.is_set(0));

    p.reset(0);
    assert_eq!(DEFAULT[0], p.current[0]);
    assert!(!p.mask.is_set(0));
    assert_eq!(0, p.mask.count());
}

#[test]
fn dynamic_palette_reset_all() {
    let mut p = DynamicPalette::default();
    let new_color = rgb(255, 0, 0);

    p.set(0, new_color);
    p.set(5, new_color);
    p.set(10, new_color);
    assert_eq!(3, p.mask.count());

    p.reset_all();
    assert_eq!(DEFAULT, p.current);
    assert_eq!(DEFAULT, *p.original());
    assert_eq!(0, p.mask.count());
}

#[test]
fn dynamic_palette_change_default_with_no_changes() {
    let mut p = DynamicPalette::default();
    let mut new_palette = DEFAULT;
    new_palette[0] = rgb(100, 100, 100);

    p.change_default(&new_palette).unwrap();
    assert_eq!(new_palette, *p.original());
    assert_eq!(new_palette, p.current);
    assert_eq!(0, p.mask.count());
}

#[test]
fn dynamic_palette_change_default_shares_the_built_in_default() {
    let mut new_palette = DEFAULT;
    new_palette[0] = rgb(100, 100, 100);

    // A custom default is an owned copy.
    let mut p = DynamicPalette::init(&new_palette).unwrap();
    assert!(!p.original_is_shared());
    assert_eq!(new_palette, *p.original());

    // Changing the custom default reuses the copy: no allocation.
    new_palette[1] = rgb(101, 101, 101);
    let (changed, stats) = with_failing(0, || p.change_default(&new_palette));
    assert_eq!(Ok(()), changed);
    assert!(!stats.has_induced_failure);
    assert_eq!(new_palette, *p.original());

    // Changing back to the built-in default releases the copy and shares it again.
    p.change_default(&DEFAULT).unwrap();
    assert!(p.original_is_shared());
    assert_eq!(DEFAULT, p.current);

    // resetDefault does the same directly, preserving changed values, and never allocates.
    p.change_default(&new_palette).unwrap();
    p.set(2, rgb(1, 2, 3));
    let ((), stats) = with_failing(0, || p.reset_default());
    assert!(!stats.has_induced_failure);
    assert!(p.original_is_shared());
    assert_eq!(DEFAULT[0], p.current[0]);
    assert_eq!(rgb(1, 2, 3), p.current[2]);
    assert!(p.mask.is_set(2));

    // The built-in default never allocates.
    let (q, stats) = with_failing(0, || DynamicPalette::init(&DEFAULT));
    assert!(q.unwrap().original_is_shared());
    assert!(!stats.has_induced_failure);
}

/// On allocation failure nothing changes.
#[test]
fn dynamic_palette_change_default_failure_changes_nothing() {
    let mut p = DynamicPalette::default();
    p.set(3, rgb(9, 9, 9));
    let mut new_palette = DEFAULT;
    new_palette[0] = rgb(100, 100, 100);
    let (changed, stats) = with_failing(0, || p.change_default(&new_palette));
    assert_eq!(Err(OutOfMemory), changed);
    assert!(stats.has_induced_failure);
    assert!(p.original_is_shared());
    assert_eq!(DEFAULT[0], p.current[0]);
    assert_eq!(rgb(9, 9, 9), p.current[3]);
}

#[test]
fn dynamic_palette_change_default_preserves_changes() {
    let mut p = DynamicPalette::default();
    let custom_color = rgb(255, 0, 0);

    p.set(5, custom_color);
    assert!(p.mask.is_set(5));

    let mut new_palette = DEFAULT;
    new_palette[0] = rgb(100, 100, 100);
    new_palette[5] = rgb(50, 50, 50);

    p.change_default(&new_palette).unwrap();

    assert_eq!(new_palette, *p.original());
    assert_eq!(new_palette[0], p.current[0]);
    assert_eq!(custom_color, p.current[5]);
    assert!(p.mask.is_set(5));
    assert_eq!(1, p.mask.count());
}

#[test]
fn dynamic_palette_change_default_with_multiple_changes() {
    let mut p = DynamicPalette::default();
    let red = rgb(255, 0, 0);
    let green = rgb(0, 255, 0);
    let blue = rgb(0, 0, 255);

    p.set(1, red);
    p.set(2, green);
    p.set(3, blue);

    let mut new_palette = DEFAULT;
    new_palette[0] = rgb(50, 50, 50);
    new_palette[1] = rgb(60, 60, 60);

    p.change_default(&new_palette).unwrap();

    assert_eq!(new_palette[0], p.current[0]);
    assert_eq!(red, p.current[1]);
    assert_eq!(green, p.current[2]);
    assert_eq!(blue, p.current[3]);
    assert_eq!(3, p.mask.count());
}

#[test]
fn dynamic_rgb() {
    let mut c = DynamicRGB::init(rgb(1, 2, 3));
    assert_eq!(Some(rgb(1, 2, 3)), c.get());
    c.set(rgb(4, 5, 6));
    assert_eq!(Some(rgb(4, 5, 6)), c.get());
    c.reset();
    assert_eq!(Some(rgb(1, 2, 3)), c.get());
    assert_eq!(None, DynamicRGB::UNSET.get());
    assert_eq!(DynamicRGB::UNSET, DynamicRGB::default());
}

#[test]
fn palette_mask_iterates_in_order() {
    let mut mask = PaletteMask::init_empty();
    for idx in [255, 0, 64, 63, 128] {
        mask.set(idx);
    }
    assert_eq!(vec![0, 63, 64, 128, 255], mask.iter_set().collect::<Vec<_>>());
    mask.unset(64);
    assert_eq!(4, mask.count());
}
