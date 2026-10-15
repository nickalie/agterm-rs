//! Kitty graphics in the terminal element (plan > Task 16; Technical Details > Frame): the images of a frame's
//! [`KittyView`] as GPUI images cached per view by `(id, generation)`, and their placements drawn in Ghostty's three
//! layers — below the cell backgrounds, between the backgrounds and the text, above the text.
//!
//! Ported from Ghostty's renderer (<https://github.com/ghostty-org/ghostty>, MIT, Copyright Mitchell Hashimoto and
//! Ghostty contributors: `src/renderer/image.zig` `kittyUpdate`/`draw`, `shaders/glsl/image.v.glsl`) by way of
//! qwertty-term (<https://github.com/joshka/qwertty-term>, MIT, Copyright (c) 2026 Josh `McKinney`: the z sort and
//! layer split of `qwertty-term-renderer`, and the eviction of images the terminal no longer holds in
//! `prepare_image_frame`).
//!
//! - **Geometry**: a placement lands at its viewport cell times the cell size plus its offset inside that cell
//!   (clamped to the cell, Ghostty's `cellOffset`), at the pixel size and from the source rectangle libghostty-vt
//!   resolved; a placement scrolled partly off the top has a negative row and is clipped by the element. The pixels
//!   are the terminal's (the owner's cells): a viewer drawing at its own font size scales them by its cell over the
//!   terminal's. Virtual placements (Unicode placeholders) have no position of their own in the C API and are not
//!   drawn.
//! - **Order**: by `z`, then image id (Ghostty's `zLessThan`), each layer drawn where Ghostty draws it.
//! - **Images**: GPUI's `RenderImage` holds BGRA with straight alpha; each image is converted once per
//!   `(id, generation)`, RGB and gray expanded, and scaled down to [`MAX_TEXTURE_SIDE`] when larger (the atlas cannot
//!   hold more; Ghostty's limit is 10000). The source rectangle is drawn by painting the whole image scaled so that
//!   rectangle covers the placement, clipped to it ([`image_bounds`]): GPUI then samples only that part of the tile.
//! - **Atlas**: a view keeps what it painted into its window's sprite atlas ([`ViewImages`]) and drops an image from
//!   the atlas once it stops drawing it (deleted, replaced by a new generation, scrolled out) and all of them when the
//!   view goes.

use std::collections::HashMap;
use std::sync::Arc;

use agterm_term::{Frame, KittyFormat, KittyImage, KittyImageKey, KittyLayer, KittyView, SourceRect};
use gpui_kit::{Bounds, Context, Corners, ImageId, Pixels, RenderImage, Subscription, Window, point, px, size};

use super::background::{BackgroundPaint, Spec};
use super::{CellMetrics, TerminalView};

/// The widest or tallest texture an image becomes: wgpu's default `max_texture_dimension_2d`, which every adapter
/// GPUI renders with supports.
pub const MAX_TEXTURE_SIDE: u32 = 8192;

/// A rectangle in device pixels, from the grid's top-left corner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeviceRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where one placement is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacementGeometry {
    pub image: KittyImageKey,
    pub layer: KittyLayer,
    pub z: i32,
    /// The rectangle drawn (device pixels from the grid's top-left corner; negative when scrolled partly off).
    pub dest: DeviceRect,
    /// The part of the image drawn, in the image's pixels.
    pub source: SourceRect,
}

/// The cell a frame's pixel geometry was computed with (the terminal's, the owner view's) and the cell a view draws
/// with, both in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cells {
    pub engine: (u32, u32),
    pub view: (u32, u32),
}

/// The placements of `view` a view draws, in drawing order (by `z`, then image id), skipping virtual placements,
/// placements off the viewport or without their image, and empty ones.
#[must_use]
pub fn placements(view: &KittyView, cells: Cells) -> Vec<PlacementGeometry> {
    let (engine_width, engine_height) = cells.engine;
    let scale = |view: u32, engine: u32| {
        if engine == 0 {
            1.0
        } else {
            f64::from(view) / f64::from(engine)
        }
    };
    let (sx, sy) = (scale(cells.view.0, engine_width), scale(cells.view.1, engine_height));
    let mut drawn: Vec<PlacementGeometry> = view
        .placements
        .iter()
        .filter(|placement| !placement.is_virtual && view.image(placement.image).is_some())
        .filter_map(|placement| {
            let rect = placement.rect?;
            let empty = rect.pixel_width == 0 || rect.pixel_height == 0;
            if empty || rect.source.width == 0 || rect.source.height == 0 {
                return None;
            }
            // Ghostty's `cellOffset`: the offset stays inside the first cell
            let offset_x = placement.x_offset.min(engine_width.saturating_sub(1));
            let offset_y = placement.y_offset.min(engine_height.saturating_sub(1));
            Some(PlacementGeometry {
                image: placement.image,
                layer: placement.layer,
                z: placement.z,
                dest: DeviceRect {
                    x: f64::from(rect.column) * f64::from(cells.view.0) + f64::from(offset_x) * sx,
                    y: f64::from(rect.row) * f64::from(cells.view.1) + f64::from(offset_y) * sy,
                    width: f64::from(rect.pixel_width) * sx,
                    height: f64::from(rect.pixel_height) * sy,
                },
                source: rect.source,
            })
        })
        .collect();
    // stable: placements of one image at one z keep the storage's order
    drawn.sort_by(|a, b| a.z.cmp(&b.z).then(a.image.id.cmp(&b.image.id)));
    drawn
}

/// The bounds to paint a whole image of `image` pixels at so that its `source` rectangle covers `dest` (GPUI's
/// `paint_image` then draws only the part of the tile inside `dest`).
#[must_use]
pub fn image_bounds(dest: Bounds<Pixels>, source: SourceRect, image: (u32, u32)) -> Bounds<Pixels> {
    let ratio = |dest: Pixels, source: u32| f32::from(dest) / narrow(f64::from(source.max(1)));
    let (rx, ry) = (
        ratio(dest.size.width, source.width),
        ratio(dest.size.height, source.height),
    );
    let origin = point(
        dest.origin.x - px(narrow(f64::from(source.x)) * rx),
        dest.origin.y - px(narrow(f64::from(source.y)) * ry),
    );
    Bounds::new(
        origin,
        size(px(narrow(f64::from(image.0)) * rx), px(narrow(f64::from(image.1)) * ry)),
    )
}

/// A device-pixel quantity as GPUI's `f32`.
#[expect(clippy::cast_possible_truncation, reason = "pixel lengths fit an f32")]
pub(super) fn narrow(value: f64) -> f32 {
    value as f32
}

/// One image converted for GPUI, with the size its placements' source rectangles are in.
#[derive(Clone)]
pub struct Texture {
    pub image: Arc<RenderImage>,
    /// The Kitty image's own size (the texture is smaller when it was scaled down).
    pub size: (u32, u32),
}

/// A view's images by `(id, generation)`.
#[derive(Default)]
pub struct KittyTextures {
    textures: HashMap<KittyImageKey, Texture>,
}

impl KittyTextures {
    /// Brings the cache to `view`: converts each image it has not seen, forgets the ones `view` no longer shows
    /// (deleted, replaced by a new generation, not placed any more).
    pub fn sync(&mut self, view: &KittyView) {
        self.textures
            .retain(|key, _| view.images.iter().any(|image| image.key == *key));
        for image in &view.images {
            if self.textures.contains_key(&image.key) {
                continue;
            }
            let Some(texture) = render_image(image) else {
                tracing::warn!(
                    id = image.key.id,
                    width = image.width,
                    height = image.height,
                    format = ?image.format,
                    "a Kitty image's pixels do not match its size and format"
                );
                continue;
            };
            let texture = Texture {
                image: Arc::new(texture),
                size: (image.width, image.height),
            };
            self.textures.insert(image.key, texture);
        }
    }

    #[must_use]
    pub fn get(&self, key: KittyImageKey) -> Option<&Texture> {
        self.textures.get(&key)
    }

    /// The images cached, in no order (tests).
    #[must_use]
    pub fn keys(&self) -> Vec<KittyImageKey> {
        self.textures.keys().copied().collect()
    }

    pub fn clear(&mut self) {
        self.textures.clear();
    }
}

/// The image's pixels as BGRA with straight alpha, scaled down to fit [`MAX_TEXTURE_SIDE`]; `None` when the
/// pixels do not hold `width * height` pixels of the format.
#[must_use]
pub fn render_image(image: &KittyImage) -> Option<RenderImage> {
    let bgra = bgra(image.format, &image.pixels, image.width, image.height)?;
    let buffer = image::RgbaImage::from_raw(image.width, image.height, bgra)?;
    Some(RenderImage::new(vec![image::Frame::new(fit_texture(buffer))]))
}

/// `pixels` of `format` as BGRA bytes (Ghostty's `convertReplace`: RGB opaque, gray to gray RGB).
#[must_use]
pub fn bgra(format: KittyFormat, pixels: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let count = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    let channels = match format {
        KittyFormat::Rgb => 3,
        KittyFormat::Rgba => 4,
        KittyFormat::Gray => 1,
        KittyFormat::GrayAlpha => 2,
    };
    if count == 0 || pixels.len() != count.checked_mul(channels)? {
        return None;
    }
    let mut out = Vec::with_capacity(count * 4);
    for pixel in pixels.chunks_exact(channels) {
        let [b, g, r, a] = match *pixel {
            [r, g, b, a] => [b, g, r, a],
            [r, g, b] => [b, g, r, u8::MAX],
            [gray, a] => [gray, gray, gray, a],
            [gray] => [gray, gray, gray, u8::MAX],
            _ => return None,
        };
        out.extend_from_slice(&[b, g, r, a]);
    }
    Some(out)
}

/// `buffer` scaled down (keeping its aspect) when a side exceeds [`MAX_TEXTURE_SIDE`].
#[must_use]
pub fn fit_texture(buffer: image::RgbaImage) -> image::RgbaImage {
    let (width, height) = buffer.dimensions();
    let longest = width.max(height);
    if longest <= MAX_TEXTURE_SIDE {
        return buffer;
    }
    let shrink = |side: u32| {
        let scaled = u64::from(side) * u64::from(MAX_TEXTURE_SIDE) / u64::from(longest);
        u32::try_from(scaled).unwrap_or(MAX_TEXTURE_SIDE).max(1)
    };
    image::imageops::resize(
        &buffer,
        shrink(width),
        shrink(height),
        image::imageops::FilterType::Triangle,
    )
}

/// The images a view painted into its window's sprite atlas, so it can drop the ones it stops drawing (module docs).
#[derive(Default)]
pub struct ViewImages {
    pub kitty: KittyTextures,
    drawn: HashMap<ImageId, Arc<RenderImage>>,
    release: Option<Subscription>,
}

impl ViewImages {
    /// Records what this frame draws and drops from `window`'s atlas what the last frame drew and this one does not.
    pub fn retain(&mut self, window: &mut Window, used: impl IntoIterator<Item = Arc<RenderImage>>) {
        let used: HashMap<ImageId, Arc<RenderImage>> = used.into_iter().map(|image| (image.id, image)).collect();
        for (id, image) in std::mem::take(&mut self.drawn) {
            if !used.contains_key(&id) {
                drop_from_atlas(window, image);
            }
        }
        self.drawn = used;
    }

    /// Drops everything from `window`'s atlas: the view is gone, or stopped drawing.
    pub fn release(&mut self, window: &mut Window) {
        self.kitty.clear();
        for (_, image) in std::mem::take(&mut self.drawn) {
            drop_from_atlas(window, image);
        }
    }

    /// Whether the view registered its release hook (once, at its first paint in a window).
    #[must_use]
    pub fn has_release_hook(&self) -> bool {
        self.release.is_some()
    }

    pub fn set_release_hook(&mut self, subscription: Subscription) {
        self.release = Some(subscription);
    }

    /// The images the last frame drew (tests).
    #[must_use]
    pub fn drawn(&self) -> Vec<Arc<RenderImage>> {
        self.drawn.values().cloned().collect()
    }
}

fn drop_from_atlas(window: &mut Window, image: Arc<RenderImage>) {
    if let Err(err) = window.drop_image(image) {
        tracing::debug!(%err, "an image could not be dropped from the atlas");
    }
}

/// What paint needs to draw the placements of one frame.
#[derive(Default)]
pub struct KittyPaint {
    pub placements: Vec<(PlacementGeometry, Texture)>,
}

impl KittyPaint {
    /// The frame's placements with their textures (images that failed to convert are left out).
    #[must_use]
    pub fn new(view: &KittyView, cells: Cells, textures: &KittyTextures) -> Self {
        let placements = placements(view, cells)
            .into_iter()
            .filter_map(|geometry| Some((geometry, textures.get(geometry.image)?.clone())))
            .collect();
        Self { placements }
    }

    /// Every texture drawn.
    pub fn images(&self) -> impl Iterator<Item = Arc<RenderImage>> + '_ {
        self.placements.iter().map(|(_, texture)| Arc::clone(&texture.image))
    }

    /// Paints the placements of `layer`, the grid's top-left corner at `origin`, at `scale` device pixels per
    /// logical pixel (clipped by the element's content mask).
    pub fn paint(&self, window: &mut Window, layer: KittyLayer, origin: gpui_kit::Point<Pixels>, scale: f32) {
        let logical = |device: f64| px(narrow(device / f64::from(scale)));
        for (geometry, texture) in self.placements.iter().filter(|(geometry, _)| geometry.layer == layer) {
            let dest = Bounds::new(
                point(origin.x + logical(geometry.dest.x), origin.y + logical(geometry.dest.y)),
                size(logical(geometry.dest.width), logical(geometry.dest.height)),
            );
            let whole = image_bounds(dest, geometry.source, texture.size);
            let image = Arc::clone(&texture.image);
            if let Err(err) = window.paint_image(dest, whole, Corners::default(), image, 0, false) {
                tracing::debug!(%err, id = geometry.image.id, "a Kitty placement was not painted");
            }
        }
    }
}

/// The image layers of one paint: the Kitty placements and the background image.
#[derive(Default)]
pub struct Images {
    pub kitty: KittyPaint,
    pub background: Option<BackgroundPaint>,
}

impl TerminalView {
    /// Prepares this paint's image layers from `frame` at the view's `metrics`: converts new Kitty images, asks the
    /// terminal for its background image, and drops from the window's atlas what the view no longer draws.
    pub(super) fn prepare_images(
        &mut self,
        frame: Option<&Frame>,
        metrics: &CellMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Images {
        if !self.images.has_release_hook() {
            let hook = cx.on_release_in(window, |view: &mut Self, window, _| view.images.release(window));
            self.images.set_release_hook(hook);
        }
        let kitty = if let Some(frame) = frame {
            self.images.kitty.sync(&frame.kitty);
            let view = (metrics.cell_width, metrics.cell_height);
            // the frame's pixels are the terminal's: the cell the owner laid the pane out with
            let engine = self
                .shared
                .laid
                .get()
                .map_or(view, |laid| (laid.grid.cell_width, laid.grid.cell_height));
            KittyPaint::new(&frame.kitty, Cells { engine, view }, &self.images.kitty)
        } else {
            self.images.kitty.clear();
            KittyPaint::default()
        };
        let config = self.shared.config();
        let spec = Spec::from_config(&config.background_image, config.palette.background_alpha);
        let background = self
            .shared
            .background_image(spec.clone(), cx)
            .zip(spec)
            .map(|(decoded, spec)| BackgroundPaint { decoded, spec });
        let used: Vec<Arc<RenderImage>> = kitty
            .images()
            .chain(background.iter().map(|paint| Arc::clone(&paint.decoded.texture)))
            .collect();
        self.images.retain(window, used);
        Images { kitty, background }
    }

    /// The Kitty images and the background this view draws and keeps in its window's atlas (tests).
    #[must_use]
    pub fn images(&self) -> &ViewImages {
        &self.images
    }

    /// Drops everything this view put into `window`'s atlas (teardown).
    pub(super) fn release_images(&mut self, window: &mut Window) {
        self.images.release(window);
    }
}
