//! Session and pane backgrounds on the terminals (upstream's `GhosttySurfaceView.applyWatermarkFromSession`,
//! `Ghostty/WatermarkRenderer.swift`; ported from the GTK build's `background.rs`): each terminal renders its pane's
//! effective background (the pane's override, else the session default) through its own config overlay
//! ([`ngterm_core::watermark::overlay_text`], loaded over the app's config, [`crate::deck::DeckEnv::config_for`]).
//! A color sets the terminal's `background` at the window's opacity (Settings ▸ Appearance ▸ Window, under which the
//! terminals otherwise render transparent: [`crate::chrome::Translucency`]); an image or a text its
//! `background-image*` lines, which the terminal draws
//! behind its grid. A text background is rasterized to a PNG under `<state dir>/watermarks` (cosmic-text, the shaping
//! and rasterizing GPUI's Linux text system links, in place of the GTK build's pango), bold sans-serif at 256 px on a
//! transparent canvas padded by 30 % of that, tinted with its color or the terminal foreground.
//!
//! Applied only on change: the background a terminal last took is remembered by its key, so a page sync that changes
//! nothing costs a comparison per pane. A text file no terminal uses any more is removed, which covers upstream's
//! removal on a clear and on a switch away from text; the model's commit removes a session's files on its permanent
//! close and a pane override's when its pane goes ([`ngterm_core::model::WatermarkRemoval`]). A text re-rendered to the
//! file a terminal already shows is read again by the terminal's next config, whose image is looked at on every
//! config (no second overlay without the image, as libghostty needed).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use ngterm_core::chrome::Rgb;
use ngterm_core::model::{BackgroundWatermark, Session, StatusPane, WatermarkKind};
use ngterm_core::watermark::{self, is_supported_image, is_valid_image_path, is_valid_text};

use crate::terminal::Shared;

/// The pixel size a text watermark is rasterized at; `background-image-fit` scales the image to the terminal.
pub const TEXT_FONT_PX: f32 = 256.0;
/// The widest or tallest text image: past it the text is drawn smaller (wgpu's default texture limit is 8192).
pub const MAX_IMAGE_SIDE: u32 = 16_384;

/// What a terminal renders, as far as its config overlay depends on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub watermark: Option<BackgroundWatermark>,
    /// The text image's file, for a text background.
    pub text_file: Option<PathBuf>,
    /// The tint a text background without its own color takes.
    pub foreground: Option<Rgb>,
    /// The window opacity a solid color is drawn at (Settings ▸ Appearance ▸ Window), baked into its
    /// `background-opacity` as upstream's `.color` background is, so a change of it re-applies the color; 1 for an
    /// image or text, whose opacity is their own.
    pub window_opacity: f64,
}

/// What pane `pane` of `session` should render, its text drawn under `state_dir`, tinted `foreground` without a color
/// of its own, a solid color at `window_opacity`.
#[must_use]
pub fn desired(session: &Session, pane: StatusPane, foreground: Rgb, state_dir: &Path, window_opacity: f64) -> Applied {
    let watermark = session.effective_background(pane).cloned();
    let text = watermark
        .as_ref()
        .is_some_and(|watermark| watermark.kind == WatermarkKind::Text);
    // a pane override renders to its own file, keyed by the pane identity; an inherited default to the session's
    let pane_key = session
        .pane_backgrounds
        .get(pane)
        .and_then(|_| session.background_file_key(pane));
    let text_file = text.then(|| watermark::rendered_text_path(state_dir, session.id(), pane_key.as_deref()));
    let own_color = watermark.as_ref().is_some_and(|mark| mark.color_hex.is_some());
    let color = watermark
        .as_ref()
        .is_some_and(|watermark| watermark.kind == WatermarkKind::Color);
    Applied {
        foreground: (text && !own_color).then_some(foreground),
        watermark,
        text_file,
        window_opacity: if color { window_opacity } else { 1.0 },
    }
}

/// The overlay a terminal takes for `applied` whose image resolved to `resolved` (the user's file, the rendered text,
/// nothing for a color or a failure); `""` with nothing to override.
#[must_use]
pub fn overlay(applied: &Applied, resolved: Option<&str>) -> String {
    watermark::overlay_text(applied.watermark.as_ref(), resolved, None, applied.window_opacity)
}

/// The solid color the terminal of `shared` renders as its own background (a color background), which an OSC 111
/// reset reports (`ngterm_core::osc_background::baseline`); `None` for any other.
#[must_use]
pub fn own_color(shared: &Rc<Shared>) -> Option<String> {
    APPLIED.with_borrow(|applied| {
        let (_, applied) = applied.get(&shared.key())?;
        let watermark = applied.watermark.as_ref()?;
        (watermark.kind == WatermarkKind::Color)
            .then(|| watermark.color_hex.clone())
            .flatten()
    })
}

/// The image file a background draws (upstream's `WatermarkRenderer.materialize`): the user's image when it is a
/// readable PNG or JPEG path, the rendered text image, or nothing for a color or a failure.
#[must_use]
pub fn materialize(applied: &Applied, state_dir: &Path) -> Option<String> {
    let watermark = applied.watermark.as_ref()?;
    match watermark.kind {
        WatermarkKind::Color => None,
        WatermarkKind::Image => watermark
            .image_path
            .clone()
            .filter(|path| is_valid_image_path(path) && is_supported_image(path) && Path::new(path).exists()),
        WatermarkKind::Text => {
            let text = watermark.text.as_deref().filter(|text| is_valid_text(text))?;
            let file = applied.text_file.as_deref()?;
            let color = text_color(watermark.color_hex.as_deref(), applied.foreground);
            watermark::ensure_directory(state_dir);
            render_text(text, color, file).then(|| file.to_string_lossy().into_owned())
        }
    }
}

/// A text background's tint: its own `#rrggbb`, else the terminal foreground, else white.
#[must_use]
pub fn text_color(hex: Option<&str>, foreground: Option<Rgb>) -> Rgb {
    hex.filter(|hex| ngterm_core::model::is_valid_color_hex(hex))
        .and_then(Rgb::from_hex)
        .or(foreground)
        .unwrap_or(Rgb::WHITE)
}

thread_local! {
    /// The system's fonts and the glyph cache, loaded at the first text drawn.
    static FONTS: RefCell<Option<(FontSystem, SwashCache)>> = const { RefCell::new(None) };
    /// What each live terminal last took, by its key, with the terminal.
    static APPLIED: RefCell<HashMap<u64, (Weak<Shared>, Applied)>> = RefCell::new(HashMap::new());
}

/// The text laid out at `px`, and its size.
fn shaped(fonts: &mut FontSystem, text: &str, px: f32) -> (Buffer, f32, f32) {
    let mut buffer = Buffer::new(fonts, Metrics::new(px, (px * 1.2).ceil()));
    buffer.set_size(None, None);
    let attrs = Attrs::new().family(Family::SansSerif).weight(Weight::BOLD);
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let (width, height) = buffer.layout_runs().fold((0.0_f32, 0.0_f32), |(width, height), run| {
        (width.max(run.line_w), height.max(run.line_top + run.line_height))
    });
    (buffer, width, height)
}

/// Rasterizes `text` in `color`, bold, onto a transparent PNG sized to the glyphs plus padding; translucency comes
/// from `background-image-opacity`, not the pixels. False, logged, when it cannot be drawn or written.
#[must_use = "a text not drawn leaves the terminal without its image"]
pub fn render_text(text: &str, color: Rgb, file: &Path) -> bool {
    FONTS.with_borrow_mut(|fonts| {
        let (system, cache) = fonts.get_or_insert_with(|| (FontSystem::new(), SwashCache::new()));
        let (mut buffer, mut width, mut height) = shaped(system, text, TEXT_FONT_PX);
        let mut px = TEXT_FONT_PX;
        #[expect(clippy::cast_precision_loss, reason = "the image side bound is far below 2^24")]
        let half = (MAX_IMAGE_SIDE / 2) as f32;
        if width.max(height) > half {
            px *= half / width.max(height);
            (buffer, width, height) = shaped(system, text, px);
        }
        if width <= 0.0 || height <= 0.0 {
            tracing::warn!(width, height, "watermark text has no glyphs to draw");
            return false;
        }
        let padding = px * 0.3;
        let side = |extent: f32| {
            let side = (extent + padding * 2.0).ceil().clamp(1.0, 65_535.0);
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped above")]
            let side = side as u32;
            side
        };
        let canvas = Canvas::new(side(width), side(height), padding, color);
        let canvas = canvas.draw(&mut buffer, system, cache);
        canvas.save(file)
    })
}

/// The text image being drawn: straight-alpha RGBA, one tint.
struct Canvas {
    width: u32,
    height: u32,
    offset: i32,
    color: Rgb,
    alpha: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32, padding: f32, color: Rgb) -> Self {
        #[expect(clippy::cast_possible_truncation, reason = "the padding is a few hundred pixels")]
        let offset = padding.round() as i32;
        Self {
            width,
            height,
            offset,
            color,
            alpha: vec![0; width as usize * height as usize],
        }
    }

    /// Draws `buffer`'s glyphs, each pixel's coverage composited over what is there.
    fn draw(mut self, buffer: &mut Buffer, system: &mut FontSystem, cache: &mut SwashCache) -> Self {
        let (width, height, offset) = (self.width, self.height, self.offset);
        let alpha = &mut self.alpha;
        buffer.draw(system, cache, Color::rgb(0xff, 0xff, 0xff), |x, y, w, h, color| {
            for row in 0..h {
                for col in 0..w {
                    let at = (
                        x.saturating_add(offset).saturating_add_unsigned(col),
                        y.saturating_add(offset).saturating_add_unsigned(row),
                    );
                    blend(alpha, (width, height), at, color.a());
                }
            }
        });
        self
    }

    /// Writes the image as a PNG at `file`.
    fn save(&self, file: &Path) -> bool {
        let pixels: Vec<u8> = self
            .alpha
            .iter()
            .flat_map(|alpha| [self.color.r, self.color.g, self.color.b, *alpha])
            .collect();
        let written = std::fs::File::create(file)
            .map_err(|err| err.to_string())
            .and_then(|out| {
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(out), self.width, self.height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().map_err(|err| err.to_string())?;
                writer.write_image_data(&pixels).map_err(|err| err.to_string())
            });
        match written {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(%err, file = %file.display(), "watermark text PNG write failed");
                false
            }
        }
    }
}

/// Composites `coverage` over the pixel at `at` of an `alpha` mask of `size`; a pixel off the mask is dropped.
fn blend(alpha: &mut [u8], size: (u32, u32), at: (i32, i32), coverage: u8) {
    let (Ok(x), Ok(y)) = (u32::try_from(at.0), u32::try_from(at.1)) else {
        return;
    };
    if x >= size.0 || y >= size.1 {
        return;
    }
    let index = y as usize * size.0 as usize + x as usize;
    let (over, under) = (u32::from(coverage), u32::from(alpha[index]));
    let composed = over + under * (255 - over) / 255;
    alpha[index] = u8::try_from(composed.min(255)).unwrap_or(u8::MAX);
}

/// What the terminal of `shared` should take now for `desired`, given what it took last: `None` when nothing changed
/// (or nothing was ever set and nothing is wanted), else `Some` with the stale text file it leaves behind, if any.
#[must_use]
pub fn change(shared: &Rc<Shared>, desired: &Applied) -> Option<Option<PathBuf>> {
    let previous = APPLIED.with_borrow(|applied| applied.get(&shared.key()).map(|(_, applied)| applied.clone()));
    if previous.as_ref() == Some(desired) || (previous.is_none() && desired.watermark.is_none()) {
        return None;
    }
    let stale = previous
        .and_then(|previous| previous.text_file)
        .filter(|file| desired.text_file.as_ref() != Some(file));
    Some(stale)
}

/// Records what the terminal of `shared` took, forgetting terminals that are gone, then removes `stale` unless a live
/// terminal still renders it.
pub fn remember(shared: &Rc<Shared>, applied: Applied, stale: Option<PathBuf>) {
    APPLIED.with_borrow_mut(|entries| {
        entries.retain(|_, (terminal, _)| terminal.upgrade().is_some_and(|terminal| !terminal.is_torn_down()));
        entries.insert(shared.key(), (Rc::downgrade(shared), applied));
    });
    if let Some(stale) = stale {
        let used = APPLIED.with_borrow(|entries| {
            entries
                .values()
                .any(|(_, entry)| entry.text_file.as_deref() == Some(stale.as_path()))
        });
        if !used {
            watermark::remove_rendered_file(&stale);
        }
    }
}

#[cfg(test)]
mod tests;
