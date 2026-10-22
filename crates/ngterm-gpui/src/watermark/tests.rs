//! Which background a pane renders and into which file (ported from the GTK build's `background.rs` tests), the
//! overlay it takes, and the text drawn to a PNG with cosmic-text.

use ngterm_core::model::SessionSeed;
use ngterm_core::pane_env::uuid_string;

use super::*;

fn mark(kind: WatermarkKind, color_hex: Option<&str>) -> BackgroundWatermark {
    BackgroundWatermark {
        kind,
        image_path: None,
        text: Some("PROD".to_owned()),
        color_hex: color_hex.map(str::to_owned),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    }
}

#[test]
fn a_pane_renders_its_override_or_the_default_into_the_matching_file() {
    let state = Path::new("/state");
    let mut session = Session::new(SessionSeed {
        initial_cwd: "/tmp".to_owned(),
        ..SessionSeed::default()
    });
    let foreground = Rgb::new(1, 2, 3);
    assert_eq!(
        desired(&session, StatusPane::Left, foreground, state, 1.0),
        Applied {
            watermark: None,
            text_file: None,
            foreground: None,
            window_opacity: 1.0,
        }
    );
    session.background_watermark = Some(mark(WatermarkKind::Text, None));
    let inherited = desired(&session, StatusPane::Scratch, foreground, state, 1.0);
    let id = uuid_string(session.id());
    assert_eq!(inherited.text_file, Some(state.join(format!("watermarks/{id}.png"))));
    assert_eq!(inherited.foreground, Some(foreground));

    session.pane_backgrounds.left = Some(mark(WatermarkKind::Text, Some("#ff0000")));
    let own = desired(&session, StatusPane::Left, foreground, state, 1.0);
    let key = uuid_string(session.pane_identity);
    assert_eq!(own.text_file, Some(state.join(format!("watermarks/{id}-{key}.png"))));
    assert_eq!(
        own.foreground, None,
        "a text with its own color does not follow the foreground"
    );

    session.pane_backgrounds.scratch = Some(mark(WatermarkKind::Color, Some("#102030")));
    let color = desired(&session, StatusPane::Scratch, foreground, state, 1.0);
    assert_eq!((color.text_file.clone(), color.foreground), (None, None));
    assert!(
        overlay(&color, None).contains("background = #102030"),
        "{}",
        overlay(&color, None)
    );
}

/// Upstream's `.color` background bakes the window opacity into its `background-opacity`, so a change of the opacity
/// re-applies it; an image or text keeps its own opacity and is not re-rendered on an opacity change.
#[test]
fn a_solid_color_takes_the_window_opacity_and_an_image_does_not() {
    let state = Path::new("/state");
    let mut session = Session::new(SessionSeed {
        initial_cwd: "/tmp".to_owned(),
        ..SessionSeed::default()
    });
    let foreground = Rgb::new(1, 2, 3);
    session.background_watermark = Some(mark(WatermarkKind::Color, Some("#102030")));
    let opaque = desired(&session, StatusPane::Left, foreground, state, 1.0);
    let translucent = desired(&session, StatusPane::Left, foreground, state, 0.6);
    assert_ne!(
        opaque, translucent,
        "an opacity change is a change of what the terminal renders"
    );
    assert!(overlay(&translucent, None).contains("background-opacity = 0.6"));
    session.background_watermark = Some(mark(WatermarkKind::Text, None));
    assert_eq!(
        desired(&session, StatusPane::Left, foreground, state, 1.0),
        desired(&session, StatusPane::Left, foreground, state, 0.6),
        "a text background ignores the window opacity"
    );
}

#[test]
fn a_text_overlay_points_at_its_image_and_nothing_overrides_nothing() {
    let applied = Applied {
        watermark: Some(mark(WatermarkKind::Text, None)),
        text_file: Some(PathBuf::from("/state/watermarks/S.png")),
        foreground: None,
        window_opacity: 1.0,
    };
    let text = overlay(&applied, Some("/state/watermarks/S.png"));
    assert!(text.contains("background-image = /state/watermarks/S.png"), "{text}");
    assert!(!text.contains("font-size"), "the terminal keeps its own live size");
    assert_eq!(overlay(&applied, None), "", "a text not drawn overrides nothing");
    let none = Applied {
        watermark: None,
        text_file: None,
        foreground: None,
        window_opacity: 1.0,
    };
    assert_eq!(overlay(&none, None), "");
}

#[test]
fn a_text_tint_is_its_color_else_the_foreground_else_white() {
    let gray = Rgb::new(0x80, 0x80, 0x80);
    assert_eq!(text_color(Some("#ff0000"), Some(gray)), Rgb::new(0xff, 0, 0));
    assert_eq!(text_color(Some("00ff00"), None), Rgb::new(0, 0xff, 0));
    assert_eq!(text_color(None, Some(gray)), gray);
    assert_eq!(text_color(Some("bogus"), None), Rgb::WHITE);
}

/// The size and the most opaque pixel's color of the PNG at `file`.
fn decoded(file: &Path) -> (u32, u32, [u8; 4]) {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(file).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let densest = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .max_by_key(|pixel| pixel[3])
        .copied()
        .unwrap();
    (info.width, info.height, densest)
}

#[test]
fn text_renders_to_a_tinted_png_padded_around_its_glyphs() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("w.png");
    assert!(render_text("PROD", Rgb::new(0x11, 0x22, 0x33), &file));
    let (width, height, densest) = decoded(&file);
    // the glyph run at 256 px plus 30% padding on each side
    assert!(width > height && height > 256, "{width}x{height}");
    assert_eq!(densest, [0x11, 0x22, 0x33, 0xff], "solid glyph pixels in the tint");
}

#[test]
fn a_long_text_is_drawn_smaller_to_stay_within_a_texture() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("long.png");
    assert!(render_text(&"W".repeat(watermark::MAX_TEXT_LENGTH), Rgb::WHITE, &file));
    let (width, _, _) = decoded(&file);
    assert!(width <= MAX_IMAGE_SIDE, "{width}");
}
