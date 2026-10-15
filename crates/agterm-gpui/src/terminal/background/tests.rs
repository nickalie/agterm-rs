//! The background image (plan > Task 16): Ghostty's fit and position math, tiling, the baked opacity, decoding by
//! content, the watermark overlay lines reaching a terminal's config, and the load in GPUI's test app (off the main
//! thread, shared between terminals, reloaded when the file changes, dropped with the config).

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_core::ghostty_config::{
    self, BackgroundImage, BackgroundImageFit as Fit, BackgroundImagePosition as Pos, ConfigPath,
};
use agterm_core::model::{BackgroundWatermark, WatermarkFit, WatermarkKind, WatermarkPosition};
use agterm_core::{paths, watermark};
use agterm_term::{Pane, PaneCommand, PaneConfig};
use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};

use super::{DeviceRect, MAX_TILES, MIN_REPEAT_SIDE, Spec, bake, decode, dest_size, layout, offset, repeated};
use crate::terminal::{Terminal, TerminalConfig, TerminalSpec, TerminalView, ViewRole};

const SCREEN: (f64, f64) = (1000.0, 500.0);

fn spec(fit: Fit, position: Pos, repeat: bool) -> Spec {
    Spec {
        path: "/nonexistent.png".into(),
        alpha: 1.0,
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

#[test]
fn the_opacity_is_capped_against_the_background_and_baked_into_the_alpha() {
    let config = |opacity| BackgroundImage {
        path: Some(ConfigPath {
            path: "/w.png".into(),
            optional: false,
        }),
        opacity,
        ..BackgroundImage::default()
    };
    let alpha = |opacity, background| Spec::from_config(&config(opacity), background).map(|spec| spec.alpha);
    assert_eq!(alpha(0.5, 255), Some(0.5));
    // Ghostty: min(opacity, 1 / background alpha) * background alpha
    assert_eq!(alpha(2.0, 51), Some(0.4));
    assert_eq!(alpha(20.0, 51), Some(1.0));
    assert_eq!(alpha(-1.0, 255), Some(0.0));
    assert_eq!(
        Spec::from_config(&BackgroundImage::default(), 255),
        None,
        "no path, no image"
    );

    let mut pixels = image::RgbaImage::from_raw(1, 1, vec![10, 20, 30, 200]).unwrap();
    bake(&mut pixels, 0.5);
    assert_eq!(pixels.into_raw(), [30, 20, 10, 100], "BGRA, alpha halved");
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
    let decoded = decode(&Spec {
        path: misnamed,
        ..spec(Fit::Contain, Pos::Center, false)
    })
    .unwrap();
    assert_eq!((decoded.size, decoded.tiles), ((3, 2), (1, 1)));
    assert_eq!(&decoded.texture.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255], "BGRA");

    let gif = dir.path().join("w.png");
    std::fs::write(&gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let refused = decode(&Spec {
        path: gif,
        ..spec(Fit::Contain, Pos::Center, false)
    });
    assert!(refused.unwrap_err().contains("unsupported"));
    let missing = decode(&spec(Fit::Contain, Pos::Center, false));
    assert!(missing.is_err());
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
    let spec = Spec::from_config(&terminal.background_image, terminal.palette.background_alpha).unwrap();
    assert_eq!(
        spec,
        Spec {
            path: "/pictures/prod.png".into(),
            alpha: 0.25,
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
