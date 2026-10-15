//! The terminal's `background-image*` (plan > Task 16): the image Ghostty draws behind the grid, which is also how
//! agterm's session and pane watermarks reach a terminal (core `watermark::overlay_text` writes these lines into the
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
//!   finished blend instead — the background color baked in and every texel at the background alpha — and the
//!   background color is painted only where no image is, so the surface stays as translucent as Ghostty's. Either
//!   way a change of opacity (or of the background color, when translucent) loads the image again.
//! - **Loading**: PNG or JPEG, told by the content first and the extension second, decoded off the main thread; the
//!   image shown stays up until its replacement is ready, and a file that fails to load keeps it too, as Ghostty
//!   does. A load is keyed by the file's path, modification time and length (a text watermark rendered again into
//!   the same file reloads; the GTK build had to clear the path once for libghostty to notice) and the baked
//!   factor; terminals showing the same one share its texture. The file is looked at again on every config change,
//!   never per frame.
//! - Small images are repeated into a texture of at least [`MIN_REPEAT_SIDE`] pixels when `repeat` is set, so a tiny
//!   pattern does not cost a sprite per copy; a tiled surface draws at most [`MAX_TILES`] textures.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::SystemTime;

use agterm_core::ghostty_config::{BackgroundImage, BackgroundImageFit, BackgroundImagePosition};
use agterm_term::Rgb;
use gpui_kit::{App, Bounds, Corners, Hsla, Pixels, RenderImage, Task, Window, fill, point, px, size};

use super::Shared;
use super::kitty::textures::{MAX_TEXTURE_SIDE, resample, texture_size};
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

    fn load_key(&self) -> std::io::Result<LoadKey> {
        let metadata = std::fs::metadata(&self.path)?;
        Ok(LoadKey {
            path: self.path.clone(),
            modified: metadata.modified().ok(),
            len: metadata.len(),
            alpha: self.alpha.to_bits(),
            composite: self.composite.map(|(color, alpha)| [color.r, color.g, color.b, alpha]),
            repeat: self.repeat,
        })
    }
}

/// A decoded background: the file as it was on disk and how it was baked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoadKey {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub len: u64,
    alpha: u32,
    composite: Option<[u8; 4]>,
    repeat: bool,
}

/// A background image ready to draw.
pub struct Decoded {
    pub texture: Arc<RenderImage>,
    /// The image's own size, which the fit works from.
    pub size: (u32, u32),
    /// How many copies of the image across and down the texture holds.
    pub tiles: (u32, u32),
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded")
            .field("size", &self.size)
            .field("tiles", &self.tiles)
            .finish_non_exhaustive()
    }
}

/// The backgrounds of this (the main) thread, shared by every terminal showing one: loaded, or loading for the
/// terminals waiting on them.
#[derive(Default)]
struct Registry {
    loaded: HashMap<LoadKey, std::sync::Weak<Decoded>>,
    loading: HashMap<LoadKey, Load>,
}

/// A load in flight (detached: it finishes even when no terminal waits any more) and the terminals waiting for it.
struct Load {
    waiters: Vec<Weak<Shared>>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// A terminal's background image: what the config asks for and what is loaded (module docs).
#[derive(Default)]
pub struct Loader {
    spec: Option<Spec>,
    /// The file was looked at for the current spec and config.
    checked: bool,
    current: Option<(LoadKey, Arc<Decoded>)>,
    loading: Option<LoadKey>,
}

impl Loader {
    /// The config changed: look at the file again at the next paint.
    pub fn invalidate(&mut self) {
        self.checked = false;
    }

    /// The image loaded for the current spec (or the one it replaces while loading).
    #[must_use]
    pub fn current(&self) -> Option<Arc<Decoded>> {
        self.current.as_ref().map(|(_, decoded)| Arc::clone(decoded))
    }

    /// The file of the current spec could not be looked at: whatever was loading for an earlier spec must not
    /// replace the image on screen when it finishes.
    fn lookup_failed(&mut self) {
        self.loading = None;
    }

    /// Whether this terminal waits for a load (tests).
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    fn finish(&mut self, key: &LoadKey, decoded: Option<&Arc<Decoded>>) {
        if self.loading.as_ref() == Some(key) {
            self.loading = None;
            if let Some(decoded) = decoded {
                self.current = Some((key.clone(), Arc::clone(decoded)));
            }
        }
    }
}

impl Shared {
    /// The background image to draw for `spec`, starting a load when the file (or how it is baked) changed; `None`
    /// before the first one loads.
    pub(super) fn background_image(self: &Rc<Self>, spec: Option<Spec>, cx: &App) -> Option<Arc<Decoded>> {
        let mut loader = self.background.borrow_mut();
        if loader.spec != spec {
            loader.spec.clone_from(&spec);
            loader.checked = false;
        }
        let Some(spec) = spec else {
            loader.current = None;
            loader.loading = None;
            return None;
        };
        if !loader.checked {
            loader.checked = true;
            match spec.load_key() {
                Ok(key) => request(&mut loader, self, spec, key, cx),
                Err(err) => {
                    loader.lookup_failed();
                    tracing::warn!(path = %spec.path.display(), %err, "error opening the background image file");
                }
            }
        }
        loader.current()
    }

    /// The background loader (tests).
    #[must_use]
    pub fn background_loader(&self) -> std::cell::Ref<'_, Loader> {
        self.background.borrow()
    }
}

/// Points `loader` at `key`: the image already loaded, a load in flight it joins, or a new load.
fn request(loader: &mut Loader, shared: &Rc<Shared>, spec: Spec, key: LoadKey, cx: &App) {
    if loader.current.as_ref().is_some_and(|(current, _)| *current == key) {
        loader.loading = None;
        return;
    }
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        if let Some(decoded) = registry.loaded.get(&key).and_then(std::sync::Weak::upgrade) {
            loader.loading = None;
            loader.current = Some((key, decoded));
            return;
        }
        loader.loading = Some(key.clone());
        if let Some(load) = registry.loading.get_mut(&key) {
            load.waiters.push(Rc::downgrade(shared));
            return;
        }
        spawn_load(spec, key.clone(), cx).detach();
        registry.loading.insert(
            key,
            Load {
                waiters: vec![Rc::downgrade(shared)],
            },
        );
    });
}

/// Decodes off the main thread, then hands the image to every terminal waiting for it.
fn spawn_load(spec: Spec, key: LoadKey, cx: &App) -> Task<()> {
    let decoding = cx.background_executor().spawn(async move { decode(&spec) });
    cx.foreground_executor().spawn(async move {
        let decoded = match decoding.await {
            Ok(decoded) => Some(Arc::new(decoded)),
            Err(err) => {
                tracing::warn!(path = %key.path.display(), %err, "error loading the background image");
                None
            }
        };
        let waiters = REGISTRY.with(|registry| {
            let mut registry = registry.borrow_mut();
            if let Some(decoded) = decoded.as_ref() {
                registry.loaded.retain(|_, loaded| loaded.strong_count() > 0);
                registry.loaded.insert(key.clone(), Arc::downgrade(decoded));
            }
            registry
                .loading
                .remove(&key)
                .map(|load| load.waiters)
                .unwrap_or_default()
        });
        for shared in waiters.iter().filter_map(Weak::upgrade) {
            shared.background.borrow_mut().finish(&key, decoded.as_ref());
            shared.poke();
        }
    })
}

/// Reads and decodes `spec`'s file into a texture: PNG or JPEG only (Ghostty's), the alpha baked in, scaled down to
/// fit the atlas, a small repeated image repeated into a larger texture.
///
/// # Errors
///
/// Why the file could not be read or decoded, or that its type is not PNG or JPEG.
pub fn decode(spec: &Spec) -> Result<Decoded, String> {
    let reader = image::ImageReader::open(&spec.path)
        .map_err(|err| err.to_string())?
        .with_guessed_format()
        .map_err(|err| err.to_string())?;
    match reader.format() {
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg) => {}
        Some(format) => return Err(format!("unsupported file type {format:?}")),
        None => return Err("cannot determine the file type".to_owned()),
    }
    let pixels = reader.decode().map_err(|err| err.to_string())?.into_rgba8();
    let size = pixels.dimensions();
    let mut texture = fit(pixels).ok_or_else(|| "the decoded image has no pixels".to_owned())?;
    bake(&mut texture, spec.alpha, spec.composite);
    let (texture, tiles) = if spec.repeat {
        repeated(&texture)
    } else {
        (texture, (1, 1))
    };
    Ok(Decoded {
        texture: Arc::new(RenderImage::new(vec![image::Frame::new(texture)])),
        size,
        tiles,
    })
}

/// `pixels` at their texture size (scaled down, without a full-size intermediate, past the atlas's limits).
fn fit(pixels: image::RgbaImage) -> Option<image::RgbaImage> {
    let size = pixels.dimensions();
    let target = texture_size(size.0, size.1);
    if target == size {
        return Some(pixels);
    }
    let scaled = resample(pixels.as_raw(), 4, size, target, |pixel| {
        [pixel[0], pixel[1], pixel[2], pixel[3]]
    })?;
    image::RgbaImage::from_raw(target.0, target.1, scaled)
}

/// A color channel or alpha in `0..=1` as a byte.
fn byte(value: f32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255 first"
    )]
    let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    byte
}

/// RGBA to GPUI's straight-alpha BGRA as the image is drawn at `alpha`: over an opaque background its alpha scaled;
/// with a translucent `composite` background, Ghostty's blend onto that color (`img * a * alpha + bg * (1 - a *
/// alpha)`, clamped), every texel at the background's alpha.
pub fn bake(pixels: &mut image::RgbaImage, alpha: f32, composite: Option<(Rgb, u8)>) {
    let unit = |byte: u8| f32::from(byte) / 255.0;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = *pixel;
        let coverage = unit(a) * alpha;
        *pixel = match composite {
            None => [b, g, r, byte(coverage)],
            Some((background, background_alpha)) => {
                let blend = |image: u8, background: u8| {
                    byte(unit(image) * coverage + unit(background) * (1.0 - coverage).max(0.0))
                };
                [
                    blend(b, background.b),
                    blend(g, background.g),
                    blend(r, background.r),
                    background_alpha,
                ]
            }
        };
    }
}

/// `texture` repeated across and down until each side is at least [`MIN_REPEAT_SIDE`] (and at most
/// [`MAX_TEXTURE_SIDE`]), with the copies in each direction.
#[must_use]
pub fn repeated(texture: &image::RgbaImage) -> (image::RgbaImage, (u32, u32)) {
    let (width, height) = texture.dimensions();
    let copies = |side: u32| {
        if side == 0 || side >= MIN_REPEAT_SIDE {
            1
        } else {
            MIN_REPEAT_SIDE.div_ceil(side).min(MAX_TEXTURE_SIDE / side).max(1)
        }
    };
    let (across, down) = (copies(width), copies(height));
    if (across, down) == (1, 1) {
        return (texture.clone(), (1, 1));
    }
    let mut out = image::RgbaImage::new(width * across, height * down);
    for row in 0..down {
        for col in 0..across {
            image::imageops::replace(&mut out, texture, i64::from(col * width), i64::from(row * height));
        }
    }
    (out, (across, down))
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
    /// The rectangles the background color is painted in on a `screen` (device pixels): all of it over an opaque
    /// background; under a translucent one only where no image is (the texture carries the color, module docs).
    #[must_use]
    pub fn color_areas(&self, screen: (f64, f64)) -> Vec<DeviceRect> {
        let whole = DeviceRect {
            x: 0.0,
            y: 0.0,
            width: screen.0,
            height: screen.1,
        };
        if self.spec.composite.is_none() {
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

    /// Paints the background `color` and the image over the element's `bounds` at `scale` device pixels per logical
    /// pixel.
    pub fn paint(&self, window: &mut Window, bounds: Bounds<Pixels>, scale: f32, color: Hsla) {
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
