//! The terminal's `background-image*` (plan > Task 16): the image Ghostty draws behind the grid, which is also how
//! ngterm's session and pane watermarks reach a terminal (core `watermark::overlay_text` writes these lines into the
//! terminal's config overlay; a text watermark is a rendered PNG).
//!
//! Ported from Ghostty's renderer (<https://github.com/ghostty-org/ghostty>, MIT, Copyright Mitchell Hashimoto and
//! Ghostty contributors: `src/renderer/generic.zig` `prepBackgroundImage`/`updateBgImageBuffer`,
//! `shaders/glsl/bg_image.{v,f}.glsl`):
//!
//! - **Geometry**: over the whole surface, padding included. `contain` and `cover` scale the image by the smaller or
//!   larger of the surface-to-image ratios, `stretch` takes the surface's size, `none` the image's own (one texel per
//!   device pixel); the position puts it at the start, middle or end of each axis (`center` is `center-center`);
//!   `repeat` tiles it from there both ways, otherwise the rest stays the background color.
//! - **Opacity**: Ghostty's `bg_image.f.glsl` blends the image at `background-image-opacity`, capped at
//!   `1 / background alpha`, onto an opaque copy of the background color and scales the result by the background
//!   alpha. Over an opaque background GPUI's own blending of the image (its alpha scaled by the opacity, baked into the
//!   texture) over the background color is exactly that. Under a translucent `background-opacity` the texture is the
//!   finished blend instead ([`bake`]: the background color baked in, at the background alpha, more where an opacity
//!   over 1 makes the image more opaque than that), and the background color is painted only where no image is, so
//!   the surface is exactly as translucent as Ghostty's. Paint pairs the color with the texture it was baked for.
//! - **Loading** ([`load`]): PNG or JPEG, told by the content first and the extension second, decoded off the main
//!   thread into unbaked pixels, then baked off the main thread; a change of opacity or (when translucent) of the
//!   background color re-bakes the pixels in memory. Decodes are keyed by the file's path, modification time and
//!   length (a text watermark rendered again into the same file reloads; the GTK build had to clear the path once for
//!   libghostty to notice); terminals showing the same one share it. The file is looked at again on a config change
//!   or when the spec names another file, never per frame.
//! - Small images are repeated into a texture of at least [`MIN_REPEAT_SIDE`] pixels when `repeat` is set, so a tiny
//!   pattern does not cost a sprite per copy; a tiled surface draws at most [`MAX_TILES`] textures.

pub mod load;

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{Bounds, Corners, Hsla, Pixels, Window, fill, point, px, size};
use ngterm_core::ghostty_config::{BackgroundImage, BackgroundImageFit, BackgroundImagePosition};
use ngterm_term::Rgb;

pub use self::load::{Bake, Decoded, FileKey, Loader, Source, bake, decode, repeated, texture};
use super::colors::hsla;
use super::kitty::{DeviceRect, narrow};

/// A repeated image narrower or shorter than this is repeated into one texture at least this large.
pub const MIN_REPEAT_SIDE: u32 = 256;

/// The most textures a repeated background draws.
pub const MAX_TILES: usize = 4096;

/// How a background image is drawn, from the config.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub path: PathBuf,
    /// The image opacity, capped as Ghostty caps it (module docs).
    pub alpha: f32,
    /// Under a translucent background: its color and alpha, baked into the texture (module docs).
    pub composite: Option<(Rgb, u8)>,
    pub fit: BackgroundImageFit,
    pub position: BackgroundImagePosition,
    pub repeat: bool,
}

impl Spec {
    /// The config's background image over a background of `background` color at `background_alpha`; `None` when
    /// no image is set.
    #[must_use]
    pub fn from_config(config: &BackgroundImage, background_alpha: u8, background: Rgb) -> Option<Self> {
        let path = config.path.as_ref()?.path.clone();
        let opacity = if config.opacity.is_finite() {
            config.opacity.max(0.0)
        } else {
            1.0
        };
        let (alpha, composite) = match background_alpha {
            u8::MAX => (opacity.min(1.0), None),
            0 => (opacity, Some((background, 0))),
            translucent => (
                opacity.min(255.0 / f32::from(translucent)),
                Some((background, translucent)),
            ),
        };
        Some(Self {
            path,
            alpha,
            composite,
            fit: config.fit,
            position: config.position,
            repeat: config.repeat,
        })
    }
}

/// The size the image is drawn at on a `screen` (device pixels), Ghostty's `bg_image.v.glsl`.
#[must_use]
pub fn dest_size(screen: (f64, f64), image: (u32, u32), fit: BackgroundImageFit) -> (f64, f64) {
    let (width, height) = (f64::from(image.0.max(1)), f64::from(image.1.max(1)));
    match fit {
        BackgroundImageFit::Contain => {
            let scale = (screen.0 / width).min(screen.1 / height);
            (width * scale, height * scale)
        }
        BackgroundImageFit::Cover => {
            let scale = (screen.0 / width).max(screen.1 / height);
            (width * scale, height * scale)
        }
        BackgroundImageFit::Stretch => screen,
        BackgroundImageFit::None => (width, height),
    }
}

/// Where the image of `dest` size starts on a `screen` (device pixels).
#[must_use]
pub fn offset(screen: (f64, f64), dest: (f64, f64), position: BackgroundImagePosition) -> (f64, f64) {
    use BackgroundImagePosition as P;
    let start = 0.0;
    let mid = |screen: f64, dest: f64| (screen - dest) / 2.0;
    let end = |screen: f64, dest: f64| screen - dest;
    let x = match position {
        P::TopLeft | P::CenterLeft | P::BottomLeft => start,
        P::TopCenter | P::CenterCenter | P::Center | P::BottomCenter => mid(screen.0, dest.0),
        P::TopRight | P::CenterRight | P::BottomRight => end(screen.0, dest.0),
    };
    let y = match position {
        P::TopLeft | P::TopCenter | P::TopRight => start,
        P::CenterLeft | P::CenterCenter | P::Center | P::CenterRight => mid(screen.1, dest.1),
        P::BottomLeft | P::BottomCenter | P::BottomRight => end(screen.1, dest.1),
    };
    (x, y)
}

/// The rectangles the texture is drawn at on a `screen` (device pixels from its top-left corner): one, or the tiles
/// covering it when repeated (each texture holding `tiles` copies of the image).
#[must_use]
pub fn layout(screen: (f64, f64), decoded_size: (u32, u32), tiles: (u32, u32), spec: &Spec) -> Vec<DeviceRect> {
    let dest = dest_size(screen, decoded_size, spec.fit);
    let (x, y) = offset(screen, dest, spec.position);
    if !spec.repeat {
        return vec![DeviceRect {
            x,
            y,
            width: dest.0,
            height: dest.1,
        }];
    }
    let step = (dest.0 * f64::from(tiles.0), dest.1 * f64::from(tiles.1));
    if !(step.0 >= 1e-3 && step.1 >= 1e-3) {
        return Vec::new();
    }
    let first = |offset: f64, step: f64| offset - (offset / step).ceil() * step;
    let (left, top) = (first(x, step.0), first(y, step.1));
    let mut rects = Vec::new();
    let mut row = top;
    while row < screen.1 && rects.len() < MAX_TILES {
        let mut col = left;
        while col < screen.0 && rects.len() < MAX_TILES {
            rects.push(DeviceRect {
                x: col,
                y: row,
                width: step.0,
                height: step.1,
            });
            col += step.0;
        }
        row += step.1;
    }
    rects
}

/// `rect` with its edges on whole device pixels, so neighbouring tiles and the bands around an image meet exactly.
#[must_use]
pub fn snapped(rect: DeviceRect) -> DeviceRect {
    let (left, top) = (rect.x.round(), rect.y.round());
    DeviceRect {
        x: left,
        y: top,
        width: (rect.x + rect.width).round() - left,
        height: (rect.y + rect.height).round() - top,
    }
}

/// The parts of a `screen` (device pixels) outside `covered`: up to four bands, above, below, left and right of it.
#[must_use]
pub fn uncovered(screen: (f64, f64), covered: DeviceRect) -> Vec<DeviceRect> {
    let left = covered.x.clamp(0.0, screen.0);
    let right = (covered.x + covered.width).clamp(left, screen.0);
    let top = covered.y.clamp(0.0, screen.1);
    let bottom = (covered.y + covered.height).clamp(top, screen.1);
    let band = |x: f64, y: f64, right: f64, bottom: f64| DeviceRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    };
    if right <= left || bottom <= top {
        return vec![band(0.0, 0.0, screen.0, screen.1)];
    }
    [
        band(0.0, 0.0, screen.0, top),
        band(0.0, bottom, screen.0, screen.1),
        band(0.0, top, left, bottom),
        band(right, top, screen.0, bottom),
    ]
    .into_iter()
    .filter(|band| band.width > 0.0 && band.height > 0.0)
    .collect()
}

/// What paint needs to draw the background image.
pub struct BackgroundPaint {
    pub decoded: Arc<Decoded>,
    pub spec: Spec,
}

impl BackgroundPaint {
    /// The rectangles the background color is painted in on a `screen` (device pixels): all of it when the texture
    /// was baked for an opaque background; only where no image is when it carries a translucent background's color
    /// (module docs).
    #[must_use]
    pub fn color_areas(&self, screen: (f64, f64)) -> Vec<DeviceRect> {
        let whole = DeviceRect {
            x: 0.0,
            y: 0.0,
            width: screen.0,
            height: screen.1,
        };
        if self.decoded.bake.composite.is_none() {
            return vec![whole];
        }
        if self.spec.repeat {
            return Vec::new();
        }
        let image = layout(screen, self.decoded.size, self.decoded.tiles, &self.spec);
        image
            .first()
            .map_or_else(|| vec![whole], |rect| uncovered(screen, snapped(*rect)))
    }

    /// Paints the background color and the image over the element's `bounds` at `scale` device pixels per logical
    /// pixel: `color` around a texture baked for an opaque background, the color a translucent one was baked with
    /// otherwise (until the texture for a new color is ready, the two never disagree).
    pub fn paint(&self, window: &mut Window, bounds: Bounds<Pixels>, scale: f32, color: Hsla) {
        let color = self
            .decoded
            .bake
            .composite
            .map_or(color, |(background, alpha)| hsla(background, alpha));
        let device = |logical: Pixels| f64::from(f32::from(logical)) * f64::from(scale);
        let logical = |device: f64| px(narrow(device / f64::from(scale)));
        let at = |rect: DeviceRect| {
            Bounds::new(
                point(bounds.origin.x + logical(rect.x), bounds.origin.y + logical(rect.y)),
                size(logical(rect.width), logical(rect.height)),
            )
        };
        let screen = (device(bounds.size.width), device(bounds.size.height));
        for area in self.color_areas(screen) {
            window.paint_quad(fill(at(area), color));
        }
        for rect in layout(screen, self.decoded.size, self.decoded.tiles, &self.spec) {
            let texture = Arc::clone(&self.decoded.texture);
            let painted = window.paint_image(bounds, at(snapped(rect)), Corners::default(), texture, 0, false);
            if let Err(err) = painted {
                tracing::debug!(%err, "the background image was not painted");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests;
