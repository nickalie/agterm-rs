//! Kitty graphics in the terminal element (plan > Task 16): placement geometry (cells, offsets, scrolling, a viewer's
//! own cells, source rectangles), the layer order, pixel conversion, the per-view image cache and its eviction, and
//! over a real pane in GPUI's test app: an image transmitted and placed is drawn from the window's atlas and leaves it
//! when deleted or when the terminal goes.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agterm_term::{
    KittyFormat, KittyImage, KittyImageKey, KittyLayer, KittyPlacement, KittyView, Pane, PaneCommand, PaneConfig,
    PlacementRect, SourceRect,
};
use gpui_kit::{AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, point, px, size};

use super::kitty::{
    Cells, DeviceRect, KittyTextures, MAX_TEXTURE_SIDE, bgra, fit_texture, image_bounds, placements, render_image,
};
use super::{Terminal, TerminalConfig, TerminalSpec, TerminalView, ViewRole};

fn key(id: u32, generation: u64) -> KittyImageKey {
    KittyImageKey { id, generation }
}

fn image(key: KittyImageKey, width: u32, height: u32) -> Arc<KittyImage> {
    let len = usize::try_from(width * height * 4).unwrap();
    Arc::new(KittyImage {
        key,
        number: 0,
        width,
        height,
        format: KittyFormat::Rgba,
        pixels: vec![0x80; len].into(),
    })
}

fn placed(image: KittyImageKey, z: i32, column: i32, row: i32) -> KittyPlacement {
    KittyPlacement {
        image,
        placement_id: 0,
        layer: if z < i32::MIN / 2 {
            KittyLayer::BelowBackground
        } else if z < 0 {
            KittyLayer::BelowText
        } else {
            KittyLayer::AboveText
        },
        z,
        is_virtual: false,
        x_offset: 0,
        y_offset: 0,
        rect: Some(PlacementRect {
            column,
            row,
            columns: 2,
            rows: 1,
            pixel_width: 20,
            pixel_height: 20,
            source: SourceRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
        }),
    }
}

fn view(images: Vec<Arc<KittyImage>>, placements: Vec<KittyPlacement>) -> KittyView {
    KittyView {
        generation: 1,
        images,
        placements,
        removed: Vec::new(),
    }
}

const CELLS: Cells = Cells {
    engine: (10, 20),
    view: (10, 20),
};

#[test]
fn a_placement_lands_at_its_cell_plus_its_offset_inside_the_cell() {
    let a = key(1, 1);
    let mut placement = placed(a, 0, 3, 2);
    placement.x_offset = 4;
    placement.y_offset = 7;
    let drawn = placements(&view(vec![image(a, 4, 4)], vec![placement]), CELLS);
    assert_eq!(
        drawn[0].dest,
        DeviceRect {
            x: 34.0,
            y: 47.0,
            width: 20.0,
            height: 20.0
        }
    );
    // Ghostty's cellOffset: an offset past the cell stays inside it
    placement.x_offset = 500;
    placement.y_offset = 500;
    let clamped = placements(&view(vec![image(a, 4, 4)], vec![placement]), CELLS);
    assert_eq!((clamped[0].dest.x, clamped[0].dest.y), (39.0, 59.0));
}

#[test]
fn a_placement_scrolled_partly_off_the_top_keeps_its_negative_row() {
    let a = key(1, 1);
    let drawn = placements(&view(vec![image(a, 4, 4)], vec![placed(a, 0, 0, -3)]), CELLS);
    assert_eq!((drawn[0].dest.y, drawn[0].dest.height), (-60.0, 20.0));
}

#[test]
fn a_viewer_scales_the_terminals_pixels_by_its_own_cell() {
    let a = key(1, 1);
    let mut placement = placed(a, 0, 2, 1);
    placement.x_offset = 4;
    let half = Cells {
        engine: (10, 20),
        view: (5, 10),
    };
    let drawn = placements(&view(vec![image(a, 4, 4)], vec![placement]), half);
    assert_eq!(
        drawn[0].dest,
        DeviceRect {
            x: 12.0,
            y: 10.0,
            width: 10.0,
            height: 10.0
        }
    );
}

#[test]
fn virtual_offscreen_imageless_and_empty_placements_are_not_drawn() {
    let (a, gone) = (key(1, 1), key(2, 1));
    let mut virtual_placement = placed(a, 0, 0, 0);
    virtual_placement.is_virtual = true;
    let mut offscreen = placed(a, 0, 0, 0);
    offscreen.rect = None;
    let mut empty = placed(a, 0, 0, 0);
    if let Some(rect) = empty.rect.as_mut() {
        rect.pixel_width = 0;
    }
    let shown = placed(a, 5, 1, 1);
    let all = vec![virtual_placement, offscreen, empty, placed(gone, 0, 0, 0), shown];
    let drawn = placements(&view(vec![image(a, 4, 4)], all), CELLS);
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].z, 5);
}

#[test]
fn placements_draw_by_z_then_image_id_in_three_layers() {
    let (one, two, three) = (key(1, 1), key(2, 1), key(3, 1));
    let images = vec![image(one, 4, 4), image(two, 4, 4), image(three, 4, 4)];
    let below_background = i32::MIN / 2 - 1;
    let all = vec![
        placed(three, 0, 0, 0),
        placed(two, -1, 0, 0),
        placed(one, 0, 0, 0),
        placed(two, below_background, 0, 0),
        placed(one, -1, 1, 0),
        placed(one, -1, 2, 0),
        placed(three, 7, 0, 0),
    ];
    let drawn = placements(&view(images, all), CELLS);
    let order: Vec<(i32, u32, f64)> = drawn
        .iter()
        .map(|placement| (placement.z, placement.image.id, placement.dest.x))
        .collect();
    assert_eq!(
        order,
        [
            (below_background, 2, 0.0),
            // one image at one z keeps the storage's order
            (-1, 1, 10.0),
            (-1, 1, 20.0),
            (-1, 2, 0.0),
            (0, 1, 0.0),
            (0, 3, 0.0),
            (7, 3, 0.0),
        ]
    );
    let layers: Vec<KittyLayer> = drawn.iter().map(|placement| placement.layer).collect();
    assert_eq!(
        layers,
        [
            KittyLayer::BelowBackground,
            KittyLayer::BelowText,
            KittyLayer::BelowText,
            KittyLayer::BelowText,
            KittyLayer::AboveText,
            KittyLayer::AboveText,
            KittyLayer::AboveText,
        ]
    );
}

#[test]
fn the_whole_image_is_scaled_so_its_source_rect_covers_the_placement() {
    let dest = Bounds::new(point(px(100.), px(50.)), size(px(40.), px(20.)));
    // the right half of a 4x2 image, drawn 40x20: twice the image's width, ten times its height
    let source = SourceRect {
        x: 2,
        y: 0,
        width: 2,
        height: 2,
    };
    let whole = image_bounds(dest, source, (4, 2));
    assert_eq!(whole, Bounds::new(point(px(60.), px(50.)), size(px(80.), px(20.))));
    let full = SourceRect {
        x: 0,
        y: 0,
        width: 4,
        height: 2,
    };
    assert_eq!(image_bounds(dest, full, (4, 2)), dest);
}

#[test]
fn pixels_become_bgra_with_straight_alpha() {
    assert_eq!(bgra(KittyFormat::Rgba, &[1, 2, 3, 4], 1, 1), Some(vec![3, 2, 1, 4]));
    assert_eq!(bgra(KittyFormat::Rgb, &[1, 2, 3], 1, 1), Some(vec![3, 2, 1, 255]));
    assert_eq!(
        bgra(KittyFormat::Gray, &[9, 8], 2, 1),
        Some(vec![9, 9, 9, 255, 8, 8, 8, 255])
    );
    assert_eq!(bgra(KittyFormat::GrayAlpha, &[9, 7], 1, 1), Some(vec![9, 9, 9, 7]));
    assert_eq!(bgra(KittyFormat::Rgba, &[1, 2, 3], 1, 1), None, "short");
    assert_eq!(bgra(KittyFormat::Rgb, &[], 0, 0), None, "empty");
    let rendered = render_image(&image(key(1, 1), 3, 2)).unwrap();
    assert_eq!((rendered.size(0).width.0, rendered.size(0).height.0), (3, 2));
}

#[test]
fn an_image_larger_than_the_atlas_takes_is_scaled_down() {
    let wide = image::RgbaImage::new(MAX_TEXTURE_SIDE * 2, 4);
    assert_eq!(fit_texture(wide).dimensions(), (MAX_TEXTURE_SIDE, 2));
    let fits = image::RgbaImage::new(MAX_TEXTURE_SIDE, 1);
    assert_eq!(fit_texture(fits).dimensions(), (MAX_TEXTURE_SIDE, 1));
}

#[test]
fn the_cache_converts_an_image_once_and_forgets_it_with_the_view() {
    let (a, b) = (key(1, 1), key(2, 1));
    let mut cache = KittyTextures::default();
    let both = view(
        vec![image(a, 4, 4), image(b, 2, 2)],
        vec![placed(a, 0, 0, 0), placed(b, 0, 0, 1)],
    );
    cache.sync(&both);
    let first = Arc::clone(&cache.get(a).unwrap().image);
    assert_eq!(cache.get(b).unwrap().size, (2, 2));
    cache.sync(&both);
    assert!(Arc::ptr_eq(&first, &cache.get(a).unwrap().image), "converted once");
    // `a` deleted
    cache.sync(&view(vec![image(b, 2, 2)], vec![placed(b, 0, 0, 1)]));
    assert_eq!(cache.keys(), [b]);
    // `b` transmitted again under its id: a new generation, a new texture
    let again = key(2, 2);
    cache.sync(&view(vec![image(again, 2, 2)], vec![placed(again, 0, 0, 1)]));
    assert_eq!(cache.keys(), [again]);
    // pixels that do not match the size are left out
    let broken = Arc::new(KittyImage {
        pixels: vec![0; 3].into(),
        ..(*image(key(3, 1), 4, 4)).clone()
    });
    cache.sync(&view(vec![broken], Vec::new()));
    assert!(cache.keys().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// over a real pane

const DEADLINE: Duration = Duration::from_secs(20);

fn terminal(cx: &mut TestAppContext, script: &str, dir: &Path) -> Entity<Terminal> {
    cx.executor().allow_parking();
    let mut command = PaneCommand::new("/bin/sh").arg("-c").arg(script).cwd(dir);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGTERM_") || name.starts_with("ZMX_") {
            command = command.env_remove(key);
        }
    }
    let spawn = Box::new(move |config: PaneConfig| Pane::spawn(&command, &config));
    cx.new(|cx| Terminal::new(TerminalSpec::default(), TerminalConfig::default(), spawn, cx))
}

fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for the Kitty image");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn drawn(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Vec<Arc<gpui_kit::RenderImage>> {
    view.read_with(cx, |view, _| view.images().drawn())
}

/// A 2x1 RGBA image (id 7), sent in two chunks and placed below the text at the cursor, then deleted on a line read
/// (`q=2`: no replies on the input).
const SCRIPT: &str = concat!(
    r"printf '\033_Ga=T,q=2,f=32,s=2,v=1,i=7,z=-1,m=1;AQIDBAUG\033\\\033_Gm=0;Bwg=\033\\'; ",
    r"read -r _; printf '\033_Ga=d,q=2,d=I,i=7\033\\'; exec sleep 5",
);

#[gpui_kit::test]
fn a_placed_image_is_drawn_from_the_atlas_and_leaves_it_when_deleted(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, SCRIPT, dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.simulate_resize(size(px(320.), px(200.)));
    settle(cx, |cx| !drawn(&view, cx).is_empty());
    let image = drawn(&view, cx).remove(0);
    assert_eq!((image.size(0).width.0, image.size(0).height.0), (2, 1));
    assert!(cx.update(|window, _| window.has_image_atlas_entry(&image)));
    view.read_with(cx, |view, _| {
        let frame = view.painted().expect("a frame");
        assert_eq!(frame.kitty.placements[0].layer, KittyLayer::BelowText);
        assert_eq!(view.images().kitty.keys(), [frame.kitty.images[0].key]);
    });

    let shared = terminal.read_with(cx, |terminal, _| Rc::clone(terminal.shared()));
    shared.pane().unwrap().write(b"\n").unwrap();
    settle(cx, |cx| drawn(&view, cx).is_empty());
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&image)));
    assert!(view.read_with(cx, |view, _| view.images().kitty.keys().is_empty()));
}

#[gpui_kit::test]
fn a_torn_down_terminal_drops_its_images_from_the_atlas(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let terminal = terminal(cx, SCRIPT, dir.path());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(&terminal, ViewRole::Owner, cx));
    cx.simulate_resize(size(px(320.), px(200.)));
    settle(cx, |cx| !drawn(&view, cx).is_empty());
    let image = drawn(&view, cx).remove(0);
    terminal.read_with(cx, |terminal, _| terminal.shared().teardown());
    settle(cx, |cx| drawn(&view, cx).is_empty());
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&image)));
}
