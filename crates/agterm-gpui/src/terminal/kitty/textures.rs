//! A terminal's Kitty images as GPUI textures: converted off the main thread, bounded in size, shared by the
//! terminal's views (each view still uploads into its own window's atlas, `ViewImages`).
//!
//! A program can transmit a 10000 px image (libghostty-vt's limit), so nothing about an image's size is paid on the
//! main thread: a new `(id, generation)` is converted on the background executor while placements of it draw nothing,
//! and its texture is at most [`MAX_TEXTURE_SIDE`] a side and [`MAX_TEXTURE_PIXELS`] in all, sampled straight from the
//! stored pixels into one buffer of that size (no full-size intermediate). The stored pixels themselves reach the
//! frame without a copy on the main thread too: `agterm-term` copies a new image out of the terminal on the pane's IO
//! thread after the batch that completed it, and the paint's capture only shares that copy (an image a paint catches
//! in the middle of a batch is the exception, copied at that capture).

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use agterm_term::{Frame, KittyFormat, KittyImage, KittyImageKey, KittyView};
use gpui_kit::{App, RenderImage};

use crate::terminal::Shared;

/// The widest or tallest texture an image becomes: wgpu's default `max_texture_dimension_2d`, which every adapter
/// GPUI renders with supports.
pub const MAX_TEXTURE_SIDE: u32 = 8192;

/// The most pixels a texture holds (64 MB of BGRA): larger images are scaled down to it.
pub const MAX_TEXTURE_PIXELS: u64 = 4096 * 4096;

/// The size a `width` x `height` image's texture has: itself, or scaled down (aspect kept) to fit
/// [`MAX_TEXTURE_SIDE`] and [`MAX_TEXTURE_PIXELS`].
#[must_use]
pub fn texture_size(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let side = f64::from(MAX_TEXTURE_SIDE) / w.max(h);
    #[expect(clippy::cast_precision_loss, reason = "the pixel budget is far below 2^52")]
    let area = (MAX_TEXTURE_PIXELS as f64 / (w * h)).sqrt();
    let scale = side.min(area).min(1.0);
    if scale >= 1.0 {
        return (width.max(1), height.max(1));
    }
    let shrink = |side: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a positive side no larger than the original u32"
        )]
        let scaled = (side * scale).floor().max(1.0) as u32;
        scaled
    };
    (shrink(w), shrink(h))
}

/// `pixels` (`width` x `height`, `channels` bytes each) sampled into a `target`-sized buffer of 4 bytes a pixel, each
/// pixel made by `convert` from the source pixel nearest its center; `None` when `pixels` is not that size.
pub fn resample(
    pixels: &[u8],
    channels: usize,
    (width, height): (u32, u32),
    (target_width, target_height): (u32, u32),
    convert: impl Fn(&[u8]) -> [u8; 4],
) -> Option<Vec<u8>> {
    let count = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    if count == 0 || target_width == 0 || target_height == 0 || pixels.len() != count.checked_mul(channels)? {
        return None;
    }
    let row_stride = usize::try_from(width).ok()? * channels;
    let map = |target: u32, size: u32, target_size: u32| {
        // the source pixel under the target pixel's center
        let source = (2 * u64::from(target) + 1) * u64::from(size) / (2 * u64::from(target_size));
        usize::try_from(source.min(u64::from(size) - 1)).unwrap_or(0)
    };
    let columns: Vec<usize> = (0..target_width)
        .map(|x| map(x, width, target_width) * channels)
        .collect();
    let mut out = Vec::with_capacity(usize::try_from(u64::from(target_width) * u64::from(target_height) * 4).ok()?);
    for y in 0..target_height {
        let row = &pixels[map(y, height, target_height) * row_stride..][..row_stride];
        for &column in &columns {
            out.extend_from_slice(&convert(&row[column..column + channels]));
        }
    }
    Some(out)
}

/// A Kitty pixel of `format` as BGRA with straight alpha (Ghostty's `convertReplace`: RGB opaque, gray to gray RGB).
fn to_bgra(format: KittyFormat) -> impl Fn(&[u8]) -> [u8; 4] {
    move |pixel| match (format, pixel) {
        (KittyFormat::Rgba, &[r, g, b, a]) => [b, g, r, a],
        (KittyFormat::Rgb, &[r, g, b]) => [b, g, r, u8::MAX],
        (KittyFormat::GrayAlpha, &[gray, a]) => [gray, gray, gray, a],
        (KittyFormat::Gray, &[gray]) => [gray, gray, gray, u8::MAX],
        _ => [0; 4],
    }
}

fn channels(format: KittyFormat) -> usize {
    match format {
        KittyFormat::Rgb => 3,
        KittyFormat::Rgba => 4,
        KittyFormat::Gray => 1,
        KittyFormat::GrayAlpha => 2,
    }
}

/// The image's pixels as BGRA at its [`texture_size`]; `None` when they do not hold `width * height` pixels of its
/// format.
#[must_use]
pub fn bgra(image: &KittyImage) -> Option<(Vec<u8>, (u32, u32))> {
    let target = texture_size(image.width, image.height);
    let pixels = resample(
        &image.pixels,
        channels(image.format),
        (image.width, image.height),
        target,
        to_bgra(image.format),
    )?;
    Some((pixels, target))
}

/// The image as a GPUI image (see [`bgra`]).
#[must_use]
pub fn render_image(image: &KittyImage) -> Option<RenderImage> {
    let (pixels, (width, height)) = bgra(image)?;
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// One image converted for GPUI, with the size its placements' source rectangles are in.
#[derive(Clone)]
pub struct Texture {
    pub image: Arc<RenderImage>,
    /// The Kitty image's own size (the texture is smaller when it was scaled down).
    pub size: (u32, u32),
}

enum Slot {
    Converting,
    Ready(Texture),
    /// The pixels do not match the image's size and format.
    Failed,
}

/// A terminal's textures by `(id, generation)`.
#[derive(Default)]
pub struct KittyStore {
    slots: HashMap<KittyImageKey, Slot>,
    /// The frame the store last followed.
    synced: Option<u64>,
}

impl KittyStore {
    /// Follows `view` (of frame `seq`; an older frame than the last one followed changes nothing): forgets the images
    /// it no longer shows (deleted, replaced by a new generation, not placed any more) and returns the new ones, now
    /// marked converting, for the caller to convert.
    pub fn follow(&mut self, seq: u64, view: &KittyView) -> Vec<Arc<KittyImage>> {
        if self.synced.is_some_and(|synced| synced >= seq) {
            return Vec::new();
        }
        self.synced = Some(seq);
        self.slots
            .retain(|key, _| view.images.iter().any(|image| image.key == *key));
        let mut new = Vec::new();
        for image in &view.images {
            if let std::collections::hash_map::Entry::Vacant(slot) = self.slots.entry(image.key) {
                slot.insert(Slot::Converting);
                new.push(Arc::clone(image));
            }
        }
        new
    }

    /// A conversion ended; a result for an image the store no longer follows is dropped.
    pub fn finish(&mut self, key: KittyImageKey, texture: Option<Texture>) {
        if let Some(slot) = self.slots.get_mut(&key)
            && matches!(slot, Slot::Converting)
        {
            *slot = texture.map_or(Slot::Failed, Slot::Ready);
        }
    }

    /// The texture of `key`, once converted.
    #[must_use]
    pub fn get(&self, key: KittyImageKey) -> Option<&Texture> {
        match self.slots.get(&key) {
            Some(Slot::Ready(texture)) => Some(texture),
            _ => None,
        }
    }

    /// Whether `key` is being converted.
    #[must_use]
    pub fn is_converting(&self, key: KittyImageKey) -> bool {
        matches!(self.slots.get(&key), Some(Slot::Converting))
    }

    /// The images followed, in no order (tests).
    #[must_use]
    pub fn keys(&self) -> Vec<KittyImageKey> {
        self.slots.keys().copied().collect()
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.synced = None;
    }
}

impl Shared {
    /// Brings the terminal's textures to `frame`, converting new images on the background executor (the views redraw
    /// when one is ready).
    pub(in crate::terminal) fn follow_kitty(self: &Rc<Self>, frame: &Frame, cx: &App) {
        let new = self.kitty.borrow_mut().follow(frame.seq, &frame.kitty);
        for image in new {
            convert(Rc::downgrade(self), image, cx);
        }
    }

    /// The terminal's Kitty textures.
    #[must_use]
    pub fn kitty_store(&self) -> std::cell::Ref<'_, KittyStore> {
        self.kitty.borrow()
    }
}

fn convert(shared: Weak<Shared>, image: Arc<KittyImage>, cx: &App) {
    let key = image.key;
    let size = (image.width, image.height);
    let converting = cx.background_executor().spawn(async move { render_image(&image) });
    cx.foreground_executor()
        .spawn(async move {
            let converted = converting.await;
            if converted.is_none() {
                tracing::warn!(
                    id = key.id,
                    width = size.0,
                    height = size.1,
                    "a Kitty image's pixels do not match its size and format"
                );
            }
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let texture = converted.map(|image| Texture {
                image: Arc::new(image),
                size,
            });
            shared.kitty.borrow_mut().finish(key, texture);
            shared.poke();
        })
        .detach();
}
