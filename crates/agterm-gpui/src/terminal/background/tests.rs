//! The background image (plan > Task 16): Ghostty's fit and position math, tiling, the opacity and Ghostty's blend
//! onto a translucent background baked into the texture (an opacity over 1 included), the background color painted
//! only around such an image and paired with the texture's own bake, decoding by content, the watermark overlay lines
//! reaching a terminal's config, and the loader in GPUI's test app: loads the config moved away from never land, a
//! background color change re-bakes without the file and one bake at a time, one load shared between terminals,
//! reloaded when the file changes, dropped with the config.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::{
    self, BackgroundImage, BackgroundImageFit as Fit, BackgroundImagePosition as Pos, ConfigPath,
};
use agterm_core::model::{BackgroundWatermark, WatermarkFit, WatermarkKind, WatermarkPosition};
use agterm_core::{paths, watermark};
use agterm_term::{Pane, PaneCommand, PaneConfig, Rgb};
use gpui_kit::{AppContext as _, Entity, RenderImage, TestAppContext, VisualTestContext, px, size};

use super::{
    BackgroundPaint, Bake, Decoded, DeviceRect, MAX_TILES, MIN_REPEAT_SIDE, Source, Spec, bake, decode, dest_size,
    layout, offset, repeated, snapped, texture as baked, uncovered,
};
use crate::terminal::{Shared, Terminal, TerminalConfig, TerminalSpec, TerminalView, ViewRole};

const SCREEN: (f64, f64) = (1000.0, 500.0);

fn spec(fit: Fit, position: Pos, repeat: bool) -> Spec {
    Spec {
        path: "/nonexistent.png".into(),
        alpha: 1.0,
        composite: None,
        fit,
        position,
        repeat,
    }
}

#[test]
fn fit_modes_scale_like_ghostty() {
    let image = (200, 100);
    assert_eq!(dest_size(SCREEN, image, Fit::Contain), (1000.0, 500.0));
    assert_eq!(dest_size(SCREEN, (100, 100), Fit::Contain), (500.0, 500.0));
    assert_eq!(dest_size(SCREEN, (100, 100), Fit::Cover), (1000.0, 1000.0));
    assert_eq!(dest_size(SCREEN, (100, 100), Fit::Stretch), SCREEN);
    assert_eq!(dest_size(SCREEN, (100, 100), Fit::None), (100.0, 100.0));
    // a degenerate image never divides by zero
    assert!(dest_size(SCREEN, (0, 0), Fit::Contain).0.is_finite());
}

#[test]
fn positions_put_the_image_at_the_start_middle_or_end_of_each_axis() {
    let dest = (200.0, 100.0);
    let at = |position| offset(SCREEN, dest, position);
    assert_eq!(at(Pos::TopLeft), (0.0, 0.0));
    assert_eq!(at(Pos::TopCenter), (400.0, 0.0));
    assert_eq!(at(Pos::TopRight), (800.0, 0.0));
    assert_eq!(at(Pos::CenterLeft), (0.0, 200.0));
    assert_eq!(at(Pos::CenterCenter), (400.0, 200.0));
    assert_eq!(at(Pos::Center), (400.0, 200.0), "center is center-center");
    assert_eq!(at(Pos::CenterRight), (800.0, 200.0));
    assert_eq!(at(Pos::BottomLeft), (0.0, 400.0));
    assert_eq!(at(Pos::BottomCenter), (400.0, 400.0));
    assert_eq!(at(Pos::BottomRight), (800.0, 400.0));
    // cover overflows: the middle goes negative
    assert_eq!(offset(SCREEN, (1000.0, 1000.0), Pos::Center), (0.0, -250.0));
}

#[test]
fn one_image_or_tiles_from_its_offset_both_ways() {
    let single = layout(SCREEN, (300, 200), (1, 1), &spec(Fit::None, Pos::Center, false));
    assert_eq!(
        single,
        [DeviceRect {
            x: 350.0,
            y: 150.0,
            width: 300.0,
            height: 200.0
        }]
    );
    let tiles = layout(SCREEN, (300, 200), (1, 1), &spec(Fit::None, Pos::Center, true));
    let xs: Vec<f64> = tiles
        .iter()
        .filter(|rect| (rect.y + 50.0).abs() < 1e-9)
        .map(|rect| rect.x)
        .collect();
    let ys: Vec<f64> = tiles
        .iter()
        .filter(|rect| (rect.x - 50.0).abs() < 1e-9)
        .map(|rect| rect.y)
        .collect();
    assert_eq!(xs, [-250.0, 50.0, 350.0, 650.0, 950.0]);
    assert_eq!(ys, [-50.0, 150.0, 350.0]);
    assert_eq!(tiles.len(), 15);
    assert!(tiles.contains(&single[0]), "a tile sits where the single image would");
    // a texture of 2x3 copies steps by two images across and three down
    let blocks = layout(SCREEN, (300, 200), (2, 3), &spec(Fit::None, Pos::TopLeft, true));
    assert_eq!((blocks[0].width, blocks[0].height), (600.0, 600.0));
    assert_eq!(blocks.len(), 2);
}

#[test]
fn tiling_is_bounded() {
    let tiles = layout((8000.0, 8000.0), (1, 1), (1, 1), &spec(Fit::None, Pos::TopLeft, true));
    assert_eq!(tiles.len(), MAX_TILES);
}

#[test]
fn small_repeated_images_become_one_larger_texture() {
    let mut tile = image::RgbaImage::new(4, 300);
    tile.put_pixel(0, 0, image::Rgba([1, 2, 3, 4]));
    let (texture, copies) = repeated(&tile);
    assert_eq!(copies, (MIN_REPEAT_SIDE / 4, 1));
    assert_eq!(texture.dimensions(), (MIN_REPEAT_SIDE, 300));
    assert_eq!(texture.get_pixel(4, 0), &image::Rgba([1, 2, 3, 4]), "the next copy");
    let (same, one) = repeated(&image::RgbaImage::new(300, 300));
    assert_eq!((same.dimensions(), one), ((300, 300), (1, 1)));
}

const GRAY: Rgb = Rgb { r: 100, g: 100, b: 100 };

fn config(opacity: f32) -> BackgroundImage {
    BackgroundImage {
        path: Some(ConfigPath {
            path: "/w.png".into(),
            optional: false,
        }),
        opacity,
        ..BackgroundImage::default()
    }
}

#[test]
fn the_opacity_is_capped_against_the_background() {
    let of = |opacity, background| {
        Spec::from_config(&config(opacity), background, GRAY).map(|spec| (spec.alpha, spec.composite))
    };
    assert_eq!(of(0.5, 255), Some((0.5, None)));
    assert_eq!(of(3.0, 255), Some((1.0, None)), "never more than opaque");
    assert_eq!(of(-1.0, 255), Some((0.0, None)));
    // translucent: Ghostty's min(opacity, 1 / background alpha), the background color baked in
    assert_eq!(of(0.5, 51), Some((0.5, Some((GRAY, 51)))));
    assert_eq!(of(20.0, 51), Some((5.0, Some((GRAY, 51)))));
    assert_eq!(
        Spec::from_config(&BackgroundImage::default(), 255, GRAY),
        None,
        "no path, no image"
    );
}

#[test]
fn the_texture_carries_ghosttys_blend() {
    // opaque background: GPUI blends the image, its alpha scaled, over the background color
    let mut pixels = image::RgbaImage::from_raw(1, 1, vec![10, 20, 30, 200]).unwrap();
    bake(&mut pixels, 0.5, None);
    assert_eq!(pixels.into_raw(), [30, 20, 10, 100], "BGRA, alpha halved");
    // background-opacity 0.5, image opacity 1, an opaque red pixel: the pure image color at the background alpha
    // (where blending it over the color would give alpha 0.75 with the background bleeding in)
    let half = (GRAY, 128);
    let mut opaque = image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap();
    bake(&mut opaque, 1.0, Some(half));
    assert_eq!(opaque.into_raw(), [0, 0, 255, 128]);
    // a transparent texel is the background color at its alpha
    let mut clear = image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 0]).unwrap();
    bake(&mut clear, 1.0, Some(half));
    assert_eq!(clear.into_raw(), [100, 100, 100, 128]);
    // half coverage: half image, half background
    let mut mixed = image::RgbaImage::from_raw(1, 1, vec![200, 0, 0, 255]).unwrap();
    bake(&mut mixed, 0.5, Some(half));
    assert_eq!(mixed.into_raw(), [50, 50, 150, 128]);
}

#[test]
fn an_image_opacity_over_one_makes_a_translucent_surface_more_opaque_as_in_ghostty() {
    // Ghostty's Config.zig: background-opacity 0.5 with background-image-opacity 1.5 gives 0.75, the image's own
    // color (not overexposed, not at the background's 0.5)
    let spec = Spec::from_config(&config(1.5), 128, GRAY).unwrap();
    let mut opaque = image::RgbaImage::from_raw(1, 1, vec![200, 100, 50, 255]).unwrap();
    bake(&mut opaque, spec.alpha, spec.composite);
    assert_eq!(opaque.into_raw(), [50, 100, 200, 192]);
    // a texel at half alpha reaches only 0.75 coverage: blended with the background, at the background alpha
    let mut half = image::RgbaImage::from_raw(1, 1, vec![200, 100, 52, 128]).unwrap();
    bake(&mut half, spec.alpha, spec.composite);
    assert_eq!(half.into_raw(), [64, 100, 175, 128]);
}

fn painted(spec: Spec, size: (u32, u32)) -> BackgroundPaint {
    let source = Source {
        pixels: image::RgbaImage::new(size.0, size.1),
        size,
        tiles: (1, 1),
    };
    BackgroundPaint {
        decoded: Arc::new(baked(&source, Bake::of(&spec))),
        spec,
    }
}

#[test]
fn a_translucent_background_is_painted_only_around_the_image() {
    let whole = DeviceRect {
        x: 0.0,
        y: 0.0,
        width: SCREEN.0,
        height: SCREEN.1,
    };
    let opaque = painted(spec(Fit::None, Pos::Center, false), (300, 200));
    assert_eq!(opaque.color_areas(SCREEN), [whole]);
    let translucent = Spec {
        composite: Some((GRAY, 128)),
        ..spec(Fit::None, Pos::Center, false)
    };
    let band = |x, y, width, height| DeviceRect { x, y, width, height };
    assert_eq!(
        painted(translucent.clone(), (300, 200)).color_areas(SCREEN),
        [
            band(0.0, 0.0, 1000.0, 150.0),
            band(0.0, 350.0, 1000.0, 150.0),
            band(0.0, 150.0, 350.0, 200.0),
            band(650.0, 150.0, 350.0, 200.0),
        ]
    );
    // a contain-fitted image filling the width leaves bands above and below only; tiles leave nothing
    let contain = Spec {
        fit: Fit::Contain,
        ..translucent.clone()
    };
    assert_eq!(painted(contain, (200, 50)).color_areas(SCREEN).len(), 2);
    let tiled = Spec {
        repeat: true,
        ..translucent.clone()
    };
    assert!(painted(tiled, (300, 200)).color_areas(SCREEN).is_empty());
    // the color follows the texture, not a newer spec: a texture baked for an opaque background under a spec that
    // turned translucent is still painted over the whole color, and the other way round
    let opaque_texture = BackgroundPaint {
        spec: translucent.clone(),
        ..painted(spec(Fit::None, Pos::Center, false), (300, 200))
    };
    assert_eq!(opaque_texture.color_areas(SCREEN), [whole]);
    let translucent_texture = BackgroundPaint {
        spec: spec(Fit::None, Pos::Center, false),
        ..painted(translucent, (300, 200))
    };
    assert_eq!(translucent_texture.color_areas(SCREEN).len(), 4);
}

#[test]
fn image_edges_land_on_whole_device_pixels() {
    let rect = DeviceRect {
        x: 10.4,
        y: -0.6,
        width: 20.3,
        height: 5.2,
    };
    assert_eq!(
        snapped(rect),
        DeviceRect {
            x: 10.0,
            y: -1.0,
            width: 21.0,
            height: 6.0
        }
    );
    let outside = DeviceRect { x: 2000.0, ..rect };
    assert_eq!(
        uncovered(SCREEN, outside).len(),
        1,
        "an image off the surface leaves all of it"
    );
}

fn write_png(path: &Path, width: u32, height: u32) {
    let pixels = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]));
    pixels.save_with_format(path, image::ImageFormat::Png).unwrap();
}

#[test]
fn files_are_decoded_by_content_png_and_jpeg_only() {
    let dir = tempfile::tempdir().unwrap();
    // a PNG named .jpg: the content wins
    let misnamed = dir.path().join("w.jpg");
    write_png(&misnamed, 3, 2);
    let source = decode(&misnamed, false).unwrap();
    assert_eq!((source.size, source.tiles), ((3, 2), (1, 1)));
    let decoded = baked(&source, Bake::of(&spec(Fit::Contain, Pos::Center, false)));
    assert_eq!(&decoded.texture.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255], "BGRA");

    let gif = dir.path().join("w.png");
    std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    assert!(decode(&gif, false).unwrap_err().contains("unsupported"));
    assert!(decode(Path::new("/nonexistent.png"), false).is_err());
}

#[test]
fn the_watermark_overlay_lines_reach_the_terminal_config() {
    let state = tempfile::tempdir().unwrap();
    let env = paths::MapEnv::new()
        .with("HOME", "/nonexistent")
        .with(paths::STATE_DIR_ENV, state.path());
    let mark = BackgroundWatermark {
        kind: WatermarkKind::Image,
        image_path: Some("/pictures/prod.png".to_owned()),
        text: None,
        color_hex: None,
        opacity: Some(0.25),
        fit: Some(WatermarkFit::Cover),
        position: Some(WatermarkPosition::BottomRight),
        repeats: Some(true),
    };
    let overlay = watermark::overlay_text(Some(&mark), mark.image_path.as_deref(), None, 1.0);
    let config = ghostty_config::load_app_config(&env, true, Some(&overlay)).finalize(false, &[]);
    let terminal = TerminalConfig::from_ghostty(&config.config);
    let spec = Spec::from_config(
        &terminal.background_image,
        terminal.palette.background_alpha,
        terminal.theme.background,
    )
    .unwrap();
    assert_eq!(
        spec,
        Spec {
            path: "/pictures/prod.png".into(),
            alpha: 0.25,
            composite: None,
            fit: Fit::Cover,
            position: Pos::BottomRight,
            repeat: true,
        }
    );
}

// ---------------------------------------------------------------------------------------------------------------
// the load in GPUI's test app

const DEADLINE: Duration = Duration::from_secs(20);

fn config_with(path: Option<&Path>) -> TerminalConfig {
    let mut config = TerminalConfig::default();
    config.background_image.path = path.map(|path| ConfigPath {
        path: path.to_path_buf(),
        optional: false,
    });
    config
}

fn terminal(cx: &mut TestAppContext, config: TerminalConfig, dir: &Path) -> Entity<Terminal> {
    cx.executor().allow_parking();
    let mut command = PaneCommand::new("/bin/sh").arg("-c").arg("exec sleep 5").cwd(dir);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
            command = command.env_remove(key);
        }
    }
    let spawn = Box::new(move |config: PaneConfig| Pane::spawn(&command, &config));
    cx.new(|cx| Terminal::new(TerminalSpec::default(), config, spawn, cx))
}

fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for the background image");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn texture(terminal: &Entity<Terminal>, cx: &mut VisualTestContext) -> Option<Arc<gpui_kit::RenderImage>> {
    terminal.read_with(cx, |terminal, _| {
        terminal
            .shared()
            .background_loader()
            .current()
            .map(|decoded| Arc::clone(&decoded.texture))
    })
}

/// Two terminals side by side.
struct Two {
    left: Entity<TerminalView>,
    right: Entity<TerminalView>,
}

impl gpui_kit::Render for Two {
    fn render(&mut self, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<Self>) -> impl gpui_kit::IntoElement {
        use gpui_kit::{ParentElement as _, Styled as _};
        gpui_kit::div()
            .size_full()
            .flex()
            .child(gpui_kit::div().w(px(200.)).h(px(100.)).child(self.left.clone()))
            .child(gpui_kit::div().w(px(200.)).h(px(100.)).child(self.right.clone()))
    }
}

#[gpui_kit::test]
fn a_background_loads_once_for_every_terminal_and_follows_its_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("w.png");
    write_png(&file, 8, 4);
    let left = terminal(cx, config_with(Some(&file)), dir.path());
    let right = terminal(cx, config_with(Some(&file)), dir.path());
    let (two, cx) = cx.add_window_view(|_, cx| Two {
        left: cx.new(|cx| TerminalView::new(&left, ViewRole::Owner, cx)),
        right: cx.new(|cx| TerminalView::new(&right, ViewRole::Owner, cx)),
    });
    cx.simulate_resize(size(px(400.), px(100.)));
    settle(cx, |cx| texture(&left, cx).is_some() && texture(&right, cx).is_some());
    let first = texture(&left, cx).unwrap();
    assert!(
        Arc::ptr_eq(&first, &texture(&right, cx).unwrap()),
        "one decode, one texture"
    );
    let view = two.read_with(cx, |two, _| two.left.clone());
    settle(cx, |cx| {
        view.read_with(cx, |view, _| {
            view.images().drawn().iter().any(|image| Arc::ptr_eq(image, &first))
        })
    });
    assert!(cx.update(|window, _| window.has_image_atlas_entry(&first)));

    // the file rendered anew (a text watermark's new text): the next config change reloads it
    write_png(&file, 16, 4);
    left.update(cx, |terminal, cx| terminal.set_config(config_with(Some(&file)), cx));
    settle(cx, |cx| {
        texture(&left, cx).is_some_and(|texture| !Arc::ptr_eq(&texture, &first))
    });
    let reloaded = texture(&left, cx).unwrap();
    assert_eq!(reloaded.size(0).width.0, 16);

    // no image any more: the texture leaves the view and the window's atlas
    left.update(cx, |terminal, cx| terminal.set_config(config_with(None), cx));
    settle(cx, |cx| {
        texture(&left, cx).is_none() && view.read_with(cx, |view, _| view.images().drawn().is_empty())
    });
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&reloaded)));
    let shared = left.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!(!shared.background_loader().is_loading());
}

#[gpui_kit::test]
fn a_missing_file_draws_no_image(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("missing.png");
    let terminal = terminal(cx, config_with(Some(&file)), dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.simulate_resize(size(px(200.), px(100.)));
    cx.run_until_parked();
    assert!(texture(&terminal, cx).is_none());
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    assert!(!shared.background_loader().is_loading(), "nothing to load");
    assert!(view.read_with(cx, |view, _| view.images().drawn().is_empty()));
}

// ---------------------------------------------------------------------------------------------------------------
// the loader, driven through `Shared::background_image` in GPUI's test app (its background executor runs only when
// the test parks, so what is in flight is under the test's control)

fn spec_of(path: &Path, composite: Option<(Rgb, u8)>) -> Spec {
    Spec {
        path: path.to_path_buf(),
        composite,
        ..spec(Fit::Contain, Pos::Center, false)
    }
}

/// A terminal with no view (no pane): only its background loader is used.
fn idle_terminal(cx: &mut TestAppContext) -> (Entity<Terminal>, Rc<Shared>) {
    let spawn =
        Box::new(|_: PaneConfig| -> Result<Pane, agterm_term::PaneError> { unreachable!("no view lays it out") });
    let terminal = cx.new(|cx| Terminal::new(TerminalSpec::default(), TerminalConfig::default(), spawn, cx));
    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    (terminal, shared)
}

fn show(shared: &Rc<Shared>, spec: &Spec, cx: &mut TestAppContext) -> Option<Arc<Decoded>> {
    cx.update(|cx| shared.background_image(Some(spec.clone()), cx))
}

/// Shows `spec` until its texture is on screen (a decode, then a bake).
fn shown(shared: &Rc<Shared>, spec: &Spec, cx: &mut TestAppContext) -> Arc<Decoded> {
    for _ in 0..4 {
        show(shared, spec, cx);
        cx.run_until_parked();
    }
    show(shared, spec, cx).expect("the background loaded")
}

#[gpui_kit::test]
fn a_file_the_config_moved_to_that_cannot_be_read_keeps_the_image_on_screen(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, a) = (dir.path().join("c.png"), dir.path().join("a.png"));
    write_png(&c, 4, 1);
    write_png(&a, 8, 1);
    let (_terminal, shared) = idle_terminal(cx);
    let on_screen = shared_texture(&shown(&shared, &spec_of(&c, None), cx));
    // A decoded, its bake in flight, when the config moves on to B, whose file is missing
    show(&shared, &spec_of(&a, None), cx);
    cx.run_until_parked();
    show(&shared, &spec_of(&a, None), cx);
    assert!(
        shared.background_loader().pending_bake().is_some(),
        "A's bake is in flight"
    );
    show(&shared, &spec_of(&dir.path().join("b.png"), None), cx);
    cx.run_until_parked();
    let current = shared.background_loader().current().unwrap();
    assert!(Arc::ptr_eq(&current.texture, &on_screen), "C stays, A never lands");
}

#[gpui_kit::test]
fn a_file_that_does_not_decode_is_tried_once_and_keeps_the_image_on_screen(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, bad) = (dir.path().join("c.png"), dir.path().join("bad.png"));
    write_png(&c, 4, 1);
    std::fs::write(&bad, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let (terminal, shared) = idle_terminal(cx);
    let on_screen = shared_texture(&shown(&shared, &spec_of(&c, None), cx));
    let before = super::load::decodes_started();
    // every paint asks again, and the failure pokes the next one: no decode after the first
    for _ in 0..8 {
        show(&shared, &spec_of(&bad, None), cx);
        cx.run_until_parked();
    }
    assert_eq!(super::load::decodes_started() - before, 1, "decoded once");
    assert!(!shared.background_loader().is_loading(), "nothing in flight");
    let current = shared.background_loader().current().unwrap();
    assert!(Arc::ptr_eq(&current.texture, &on_screen), "C stays");
    // the file replaced with a valid image: the next config change looks at it again and loads it
    write_png(&bad, 8, 1);
    terminal.update(cx, |terminal, cx| terminal.set_config(TerminalConfig::default(), cx));
    let replaced = shown(&shared, &spec_of(&bad, None), cx);
    assert_eq!(replaced.size, (8, 1));
    assert_eq!(super::load::decodes_started() - before, 2);
}

#[gpui_kit::test]
fn a_config_change_forgets_the_loads_in_flight_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, a) = (dir.path().join("c.png"), dir.path().join("a.png"));
    write_png(&c, 4, 1);
    write_png(&a, 8, 1);
    let (terminal, shared) = idle_terminal(cx);
    let on_screen = shared_texture(&shown(&shared, &spec_of(&c, None), cx));
    show(&shared, &spec_of(&a, None), cx);
    cx.run_until_parked();
    show(&shared, &spec_of(&a, None), cx);
    // a new config (another image, say) before any paint: A's bake finishes after it and must not land
    terminal.update(cx, |terminal, cx| terminal.set_config(TerminalConfig::default(), cx));
    cx.run_until_parked();
    let current = shared.background_loader().current().unwrap();
    assert!(
        Arc::ptr_eq(&current.texture, &on_screen),
        "C stays until the next paint asks for more"
    );
    assert!(!shared.background_loader().is_loading());
}

#[gpui_kit::test]
fn a_background_color_change_rebakes_without_reading_the_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("w.png");
    write_png(&file, 4, 1);
    let (_terminal, shared) = idle_terminal(cx);
    let red = Rgb { r: 255, g: 0, b: 0 };
    let blue = Rgb { r: 0, g: 0, b: 255 };
    let green = Rgb { r: 0, g: 255, b: 0 };
    let first = shown(&shared, &spec_of(&file, Some((red, 128))), cx);
    assert_eq!(first.bake.composite, Some((red, 128)));
    let source = shared.background_loader().source().unwrap();
    // the file is gone: a re-bake must not need it
    std::fs::remove_file(&file).unwrap();
    show(&shared, &spec_of(&file, Some((blue, 128))), cx);
    assert_eq!(
        shared.background_loader().pending_bake().map(|bake| bake.composite),
        Some(Some((blue, 128)))
    );
    // a program cycling colors: one bake in flight, the newest waits for it
    show(&shared, &spec_of(&file, Some((green, 128))), cx);
    assert_eq!(super::load::bakes_in_flight(), 1);
    assert_eq!(
        shared.background_loader().pending_bake().map(|bake| bake.composite),
        Some(Some((blue, 128)))
    );
    cx.run_until_parked();
    assert_eq!(
        shared.background_loader().current().unwrap().bake.composite,
        Some((blue, 128))
    );
    let last = shown(&shared, &spec_of(&file, Some((green, 128))), cx);
    assert_eq!(last.bake.composite, Some((green, 128)));
    assert!(
        Arc::ptr_eq(&shared.background_loader().source().unwrap(), &source),
        "decoded once"
    );
}

fn shared_texture(decoded: &Arc<Decoded>) -> Arc<RenderImage> {
    Arc::clone(&decoded.texture)
}
