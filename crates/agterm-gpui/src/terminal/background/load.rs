//! Loading a terminal's background image in two steps, both off the main thread and shared by every terminal on it:
//! the file decoded into its unbaked pixels (a [`Source`], keyed by the file as it is on disk), then those pixels
//! baked into a texture for an opacity and, under a translucent background, a background color (a [`Decoded`], keyed
//! by the source and the [`Bake`]). A background color change (OSC 11) under translucency therefore re-bakes the
//! pixels already in memory and never reads the file again; a terminal has at most one bake in flight, and a newer
//! bake waits for it, so a program cycling colors cannot pile up work.
//!
//! The image on screen stays up until its replacement is ready. A config change forgets the loads in flight for the
//! config before it at once, so none of them can replace the image on screen when it finishes; a file that cannot be
//! looked at, or that does not decode, keeps the image on screen too, as Ghostty does; a file that does not decode
//! is not tried again until the file is looked at again (the config changed) and has changed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::SystemTime;

use agterm_term::Rgb;
use gpui_kit::{App, RenderImage};

use super::super::Shared;
use super::super::kitty::textures::{MAX_TEXTURE_SIDE, resample, texture_size};
use super::{MIN_REPEAT_SIDE, Spec};

/// A background file as it is on disk, and whether it is repeated (small repeated images are tiled when decoded).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub len: u64,
    pub repeat: bool,
}

impl FileKey {
    /// `spec`'s file as it is now.
    ///
    /// # Errors
    ///
    /// The file cannot be looked at.
    pub fn of(spec: &Spec) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(&spec.path)?;
        Ok(Self {
            path: spec.path.clone(),
            modified: metadata.modified().ok(),
            len: metadata.len(),
            repeat: spec.repeat,
        })
    }
}

/// How decoded pixels are baked into a texture (module docs of `background`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bake {
    alpha: u32,
    pub composite: Option<(Rgb, u8)>,
}

impl Bake {
    #[must_use]
    pub fn of(spec: &Spec) -> Self {
        Self {
            alpha: spec.alpha.to_bits(),
            composite: spec.composite,
        }
    }

    #[must_use]
    pub fn alpha(self) -> f32 {
        f32::from_bits(self.alpha)
    }
}

/// A decoded background file: its pixels fitted to the atlas (and tiled when small and repeated), not yet baked.
pub struct Source {
    /// RGBA.
    pub pixels: image::RgbaImage,
    /// The image's own size, which the fit works from.
    pub size: (u32, u32),
    /// How many copies of the image across and down `pixels` holds.
    pub tiles: (u32, u32),
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("size", &self.size)
            .field("tiles", &self.tiles)
            .finish_non_exhaustive()
    }
}

/// A background image ready to draw.
pub struct Decoded {
    pub texture: Arc<RenderImage>,
    /// The image's own size, which the fit works from.
    pub size: (u32, u32),
    /// How many copies of the image across and down the texture holds.
    pub tiles: (u32, u32),
    /// What the texture was baked for: paint pairs the background color with it, not with a newer spec.
    pub bake: Bake,
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded")
            .field("size", &self.size)
            .field("tiles", &self.tiles)
            .field("bake", &self.bake)
            .finish_non_exhaustive()
    }
}

/// A texture: the source it was baked from and how.
pub type TextureKey = (FileKey, Bake);

/// Results shared by the terminals of this (the main) thread: the ones made, and the ones in flight with the
/// terminals waiting for them (the work is detached: it finishes even when no terminal waits any more).
struct Flights<K, V> {
    made: HashMap<K, std::sync::Weak<V>>,
    waiting: HashMap<K, Vec<Weak<Shared>>>,
}

impl<K, V> Default for Flights<K, V> {
    fn default() -> Self {
        Self {
            made: HashMap::new(),
            waiting: HashMap::new(),
        }
    }
}

/// What a request for a result found.
enum Joined<V> {
    Made(Arc<V>),
    /// Another terminal's request is in flight: the caller waits for it.
    Waiting,
    /// The caller starts the work.
    Start,
}

impl<K: Eq + Hash + Clone, V> Flights<K, V> {
    fn join(&mut self, key: &K, shared: &Rc<Shared>) -> Joined<V> {
        if let Some(made) = self.made.get(key).and_then(std::sync::Weak::upgrade) {
            return Joined::Made(made);
        }
        let waiters = self.waiting.entry(key.clone()).or_default();
        waiters.push(Rc::downgrade(shared));
        if waiters.len() == 1 {
            Joined::Start
        } else {
            Joined::Waiting
        }
    }

    fn land(&mut self, key: &K, made: Option<&Arc<V>>) -> Vec<Weak<Shared>> {
        if let Some(made) = made {
            self.made.retain(|_, kept| kept.strong_count() > 0);
            self.made.insert(key.clone(), Arc::downgrade(made));
        }
        self.waiting.remove(key).unwrap_or_default()
    }

    /// How many requests are in flight (tests).
    fn in_flight(&self) -> usize {
        self.waiting.len()
    }
}

#[derive(Default)]
struct Registry {
    sources: Flights<FileKey, Source>,
    textures: Flights<TextureKey, Decoded>,
    /// How many decodes were started (tests).
    decodes: usize,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// The bakes in flight on this thread (tests).
#[must_use]
pub fn bakes_in_flight() -> usize {
    REGISTRY.with(|registry| registry.borrow().textures.in_flight())
}

/// How many decodes were started on this thread (tests).
#[must_use]
pub fn decodes_started() -> usize {
    REGISTRY.with(|registry| registry.borrow().decodes)
}

/// A terminal's background image: what the config asks for and what is loaded (module docs).
#[derive(Default)]
pub struct Loader {
    spec: Option<Spec>,
    /// The file was looked at for the current spec and config.
    checked: bool,
    /// The current spec's file as last looked at; `None` when it could not be.
    file: Option<FileKey>,
    source: Option<(FileKey, Arc<Source>)>,
    current: Option<(TextureKey, Arc<Decoded>)>,
    pending_source: Option<FileKey>,
    pending_bake: Option<TextureKey>,
    /// The file as it was when it last failed to decode: not decoded again until it changes or the config reloads.
    failed: Option<FileKey>,
}

impl Loader {
    /// The config changed: look at the file again at the next paint, decode it again even if it failed unchanged (the
    /// failure may not be about its contents: permissions, a transient I/O error), and let no load started for the
    /// config before replace the image on screen.
    pub fn invalidate(&mut self) {
        self.checked = false;
        self.pending_source = None;
        self.pending_bake = None;
        self.failed = None;
    }

    /// The image loaded for the current spec (or the one it replaces while loading).
    #[must_use]
    pub fn current(&self) -> Option<Arc<Decoded>> {
        self.current.as_ref().map(|(_, decoded)| Arc::clone(decoded))
    }

    /// The decoded pixels the textures are baked from (tests).
    #[must_use]
    pub fn source(&self) -> Option<Arc<Source>> {
        self.source.as_ref().map(|(_, source)| Arc::clone(source))
    }

    /// Whether this terminal waits for a decode or a bake (tests).
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.pending_source.is_some() || self.pending_bake.is_some()
    }

    /// The bake this terminal waits for (tests).
    #[must_use]
    pub fn pending_bake(&self) -> Option<Bake> {
        self.pending_bake.as_ref().map(|(_, bake)| *bake)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Takes `spec`; whether its file must be looked at again (it names another file, or the config changed).
    fn take(&mut self, spec: Option<&Spec>) -> bool {
        let file = |spec: Option<&Spec>| spec.map(|spec| (spec.path.clone(), spec.repeat));
        if file(self.spec.as_ref()) != file(spec) {
            self.checked = false;
        }
        if self.spec.as_ref() != spec {
            self.spec = spec.cloned();
        }
        !std::mem::replace(&mut self.checked, true)
    }

    fn finish_source(&mut self, key: &FileKey, source: Option<&Arc<Source>>) {
        if self.pending_source.as_ref() != Some(key) {
            return;
        }
        self.pending_source = None;
        if let Some(source) = source {
            // a bake of the previous source must not land over the new one
            self.pending_bake = None;
            self.source = Some((key.clone(), Arc::clone(source)));
        } else {
            self.failed = Some(key.clone());
        }
    }

    fn finish_bake(&mut self, key: &TextureKey, decoded: Option<&Arc<Decoded>>) {
        if self.pending_bake.as_ref() != Some(key) {
            return;
        }
        self.pending_bake = None;
        if let Some(decoded) = decoded {
            self.current = Some((key.clone(), Arc::clone(decoded)));
        }
    }
}

impl Shared {
    /// The background image to draw for `spec`, starting a decode when its file changed and a bake when how it is
    /// baked did; `None` before the first one is ready.
    pub(in crate::terminal) fn background_image(self: &Rc<Self>, spec: Option<Spec>, cx: &App) -> Option<Arc<Decoded>> {
        let mut loader = self.background.borrow_mut();
        let Some(spec) = spec else {
            loader.clear();
            return None;
        };
        if loader.take(Some(&spec)) {
            loader.file = match FileKey::of(&spec) {
                Ok(file) => Some(file),
                Err(err) => {
                    tracing::warn!(path = %spec.path.display(), %err, "error opening the background image file");
                    loader.pending_source = None;
                    loader.pending_bake = None;
                    None
                }
            };
        }
        let file = loader.file.clone()?;
        let decoded = loader.source.as_ref().is_some_and(|(source, _)| *source == file);
        let failed = loader.failed.as_ref() == Some(&file);
        if !decoded && !failed && loader.pending_source.as_ref() != Some(&file) {
            request_source(&mut loader, self, &spec, file.clone(), cx);
        }
        if let Some((source_key, source)) = loader.source.clone()
            && source_key == file
        {
            let wanted = (file, Bake::of(&spec));
            let shown = loader.current.as_ref().is_some_and(|(current, _)| *current == wanted);
            // one bake in flight at a time: a newer one waits for it (its result lands, then the next paint asks)
            if !shown && loader.pending_bake.is_none() {
                request_bake(&mut loader, self, &source, wanted, cx);
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

fn request_source(loader: &mut Loader, shared: &Rc<Shared>, spec: &Spec, key: FileKey, cx: &App) {
    let joined = REGISTRY.with(|registry| registry.borrow_mut().sources.join(&key, shared));
    match joined {
        Joined::Made(source) => {
            loader.pending_source = Some(key.clone());
            loader.finish_source(&key, Some(&source));
        }
        Joined::Waiting => loader.pending_source = Some(key),
        Joined::Start => {
            loader.pending_source = Some(key.clone());
            REGISTRY.with(|registry| registry.borrow_mut().decodes += 1);
            let path = spec.path.clone();
            let decoding = cx.background_executor().spawn(async move { decode(&path, key.repeat) });
            cx.foreground_executor()
                .spawn(async move {
                    let source = match decoding.await {
                        Ok(source) => Some(Arc::new(source)),
                        Err(err) => {
                            tracing::warn!(path = %key.path.display(), %err, "error loading the background image");
                            None
                        }
                    };
                    let waiters = REGISTRY.with(|registry| registry.borrow_mut().sources.land(&key, source.as_ref()));
                    for shared in waiters.iter().filter_map(Weak::upgrade) {
                        shared.background.borrow_mut().finish_source(&key, source.as_ref());
                        shared.poke();
                    }
                })
                .detach();
        }
    }
}

fn request_bake(loader: &mut Loader, shared: &Rc<Shared>, source: &Arc<Source>, key: TextureKey, cx: &App) {
    let joined = REGISTRY.with(|registry| registry.borrow_mut().textures.join(&key, shared));
    loader.pending_bake = Some(key.clone());
    match joined {
        Joined::Made(decoded) => loader.finish_bake(&key, Some(&decoded)),
        Joined::Waiting => {}
        Joined::Start => {
            let source = Arc::clone(source);
            let bake = key.1;
            let baking = cx.background_executor().spawn(async move { texture(&source, bake) });
            cx.foreground_executor()
                .spawn(async move {
                    let decoded = Arc::new(baking.await);
                    let waiters = REGISTRY.with(|registry| registry.borrow_mut().textures.land(&key, Some(&decoded)));
                    for shared in waiters.iter().filter_map(Weak::upgrade) {
                        shared.background.borrow_mut().finish_bake(&key, Some(&decoded));
                        shared.poke();
                    }
                })
                .detach();
        }
    }
}

/// Reads and decodes the file at `path`: PNG or JPEG only (Ghostty's), fitted to the atlas, a small image tiled into
/// a larger one when `repeat`.
///
/// # Errors
///
/// Why the file could not be read or decoded, or that its type is not PNG or JPEG.
pub fn decode(path: &std::path::Path, repeat: bool) -> Result<Source, String> {
    let reader = image::ImageReader::open(path)
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
    let fitted = fit(pixels).ok_or_else(|| "the decoded image has no pixels".to_owned())?;
    let (pixels, tiles) = if repeat { repeated(&fitted) } else { (fitted, (1, 1)) };
    Ok(Source { pixels, size, tiles })
}

/// `source` baked as `bake` says into a texture.
#[must_use]
pub fn texture(source: &Source, bake: Bake) -> Decoded {
    let mut pixels = source.pixels.clone();
    self::bake(&mut pixels, bake.alpha(), bake.composite);
    Decoded {
        texture: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
        size: source.size,
        tiles: source.tiles,
        bake,
    }
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

/// RGBA to GPUI's straight-alpha BGRA as the image is drawn at opacity `alpha` (already capped at `1 / background
/// alpha`). Over an opaque background: the alpha scaled. Under a translucent `composite` background, Ghostty's
/// `bg_image.f.glsl` in straight alpha: with `c = a * alpha` and `k = max(1, c)`, the color is
/// `(img * c + bg * max(0, 1 - c)) / k` and the alpha `background alpha * k` — the background alpha where the image
/// is at most opaque, more where an opacity over 1 makes it so (Ghostty's documented 0.5 x 1.5 = 0.75).
pub fn bake(pixels: &mut image::RgbaImage, alpha: f32, composite: Option<(Rgb, u8)>) {
    let unit = |byte: u8| f32::from(byte) / 255.0;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = *pixel;
        let coverage = unit(a) * alpha;
        *pixel = match composite {
            None => [b, g, r, byte(coverage)],
            Some((background, background_alpha)) => {
                let excess = coverage.max(1.0);
                let blend = |image: u8, background: u8| {
                    byte((unit(image) * coverage + unit(background) * (1.0 - coverage).max(0.0)) / excess)
                };
                [
                    blend(b, background.b),
                    blend(g, background.g),
                    blend(r, background.r),
                    byte(unit(background_alpha) * excess),
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
