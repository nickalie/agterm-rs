//! Session and pane backgrounds on the terminals (upstream's `GhosttySurfaceView.applyWatermarkFromSession`,
//! `Ghostty/WatermarkRenderer.swift`): each terminal renders its pane's effective background (the pane's override,
//! else the session default) through its own config overlay ([`agterm_core::watermark::overlay_text`]). A text
//! background is rasterized to a PNG under `<state dir>/watermarks` with pango, tinted with its color or the terminal
//! foreground.
//!
//! Applied only on change: the background a terminal last took is remembered by its key, so a page sync that changes
//! nothing costs a comparison per pane. A text file no terminal uses any more is removed, which covers upstream's
//! removal on a clear and on a switch away from text; a session's files outlive its permanent close.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use agterm_core::model::{BackgroundWatermark, Session, StatusPane, WatermarkKind};
use agterm_core::paths::{self, ProcessEnv};
use agterm_core::watermark::{self, is_supported_image, is_valid_image_path, is_valid_text};
use agterm_ghostty::{Color, SurfaceKey};
use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};

use crate::split::PaneView;
use crate::surface::{self, TerminalSurface};

/// The window opacity a solid color is drawn at: this build has no window translucency, so solid.
const WINDOW_OPACITY: f64 = 1.0;

/// The point size a text watermark is rasterized at; `background-image-fit` scales the image to the terminal.
const TEXT_FONT_PX: f64 = 256.0;

/// The widest or tallest text image: past it the text is drawn smaller, since a GL texture this large is the most
/// a renderer is sure to take.
const MAX_IMAGE_SIDE: i32 = 16_384;

/// What a terminal renders, as far as its config overlay depends on it.
#[derive(Debug, Clone, PartialEq)]
struct Applied {
    watermark: Option<BackgroundWatermark>,
    /// The text image's file, for a text background.
    text_file: Option<PathBuf>,
    /// The tint a text background without its own color takes.
    foreground: Option<Color>,
}

thread_local! {
    /// The background each live terminal last took, by its key.
    static APPLIED: RefCell<HashMap<SurfaceKey, Applied>> = RefCell::new(HashMap::new());
}

/// Applies `session`'s background (and its per-pane overrides) to the terminals of its page, on every page sync.
pub fn sync_page(panes: &PaneView, session: &Session) {
    let foreground = crate::ghostty_app::foreground_color();
    for pane in [StatusPane::Left, StatusPane::Right, StatusPane::Scratch] {
        let Some(token) = session
            .pane_slot(pane)
            .get()
            .map(|surface| surface.pane_token().to_owned())
        else {
            continue;
        };
        let Some(terminal) = panes.terminal(&token) else {
            continue;
        };
        let desired = desired(session, pane, foreground);
        let previous = APPLIED.with(|applied| applied.borrow().get(&terminal.key()).cloned());
        if previous.as_ref() == Some(&desired) || (previous.is_none() && desired.watermark.is_none()) {
            continue;
        }
        apply(terminal, &desired);
        let stale = previous
            .and_then(|previous| previous.text_file)
            .filter(|file| desired.text_file.as_ref() != Some(file));
        remember(terminal.key(), desired);
        if let Some(stale) = stale {
            remove_unless_used(&stale);
        }
    }
}

/// Records what the terminal `key` took, forgetting terminals that are gone.
fn remember(key: SurfaceKey, applied: Applied) {
    APPLIED.with(|entries| {
        let mut entries = entries.borrow_mut();
        entries.retain(|key, _| surface::lookup(*key).is_some());
        entries.insert(key, applied);
    });
}

/// What pane `pane` of `session` should render.
fn desired(session: &Session, pane: StatusPane, foreground: Option<Color>) -> Applied {
    let watermark = session.effective_background(pane).cloned();
    let text = watermark
        .as_ref()
        .is_some_and(|watermark| watermark.kind == WatermarkKind::Text);
    // a pane override renders to its own file, keyed by the pane identity; an inherited default to the session's
    let pane_key = session
        .pane_backgrounds
        .get(pane)
        .and_then(|_| session.background_file_key(pane));
    let text_file = text.then(|| watermark::rendered_text_path(state_dir(), session.id(), pane_key.as_deref()));
    let foreground = foreground.filter(|_| text && watermark.as_ref().is_some_and(|w| w.color_hex.is_none()));
    Applied {
        watermark,
        text_file,
        foreground,
    }
}

/// Builds the terminal's overlay for `desired`, rendering a text image first.
fn apply(terminal: &TerminalSurface, desired: &Applied) {
    let resolved = desired
        .watermark
        .as_ref()
        .and_then(|watermark| materialize(watermark, desired.text_file.as_deref(), desired.foreground));
    let overlay = watermark::overlay_text(desired.watermark.as_ref(), resolved.as_deref(), None, WINDOW_OPACITY);
    terminal.set_config_overlay(&overlay);
}

/// The image file a background draws (upstream's `WatermarkRenderer.materialize`): the user's image when it is a
/// readable PNG or JPEG path, the rendered text image, or nothing for a color or a failure.
fn materialize(watermark: &BackgroundWatermark, text_file: Option<&Path>, foreground: Option<Color>) -> Option<String> {
    match watermark.kind {
        WatermarkKind::Color => None,
        WatermarkKind::Image => watermark
            .image_path
            .clone()
            .filter(|path| is_valid_image_path(path) && is_supported_image(path) && Path::new(path).exists()),
        WatermarkKind::Text => {
            let text = watermark.text.as_deref().filter(|text| is_valid_text(text))?;
            let file = text_file?;
            let color = text_color(watermark.color_hex.as_deref(), foreground);
            watermark::ensure_directory(state_dir());
            render_text(text, &color, file).then(|| file.to_string_lossy().into_owned())
        }
    }
}

/// A text background's tint: its own `#rrggbb`, else the terminal foreground, else white.
fn text_color(hex: Option<&str>, foreground: Option<Color>) -> gdk::RGBA {
    let from_hex = hex.and_then(|hex| {
        let digits = hex.strip_prefix('#').unwrap_or(hex);
        let value = u32::from_str_radix(digits, 16).ok().filter(|_| digits.len() == 6)?;
        let [_, r, g, b] = value.to_be_bytes();
        Some(Color { r, g, b })
    });
    let color = from_hex.or(foreground).unwrap_or(Color {
        r: 0xff,
        g: 0xff,
        b: 0xff,
    });
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        1.0,
    )
}

/// Rasterizes `text` in `color`, bold, onto a transparent PNG sized to the glyphs plus padding; translucency comes
/// from `background-image-opacity`, not the pixels. False, logged, when it cannot be drawn or written.
fn render_text(text: &str, color: &gdk::RGBA, file: &Path) -> bool {
    let layout = gtk::DrawingArea::new().create_pango_layout(Some(text));
    let mut font = pango::FontDescription::new();
    font.set_family("Sans");
    font.set_weight(pango::Weight::Bold);
    let mut px = TEXT_FONT_PX;
    font.set_absolute_size(px * f64::from(pango::SCALE));
    layout.set_font_description(Some(&font));
    let (width, height) = layout.pixel_size();
    let widest = width.max(height);
    if widest > MAX_IMAGE_SIDE / 2 {
        px *= f64::from(MAX_IMAGE_SIDE / 2) / f64::from(widest);
        font.set_absolute_size(px * f64::from(pango::SCALE));
        layout.set_font_description(Some(&font));
    }
    let (width, height) = layout.pixel_size();
    let padding = px * 0.3;
    let canvas = (f64::from(width) + padding * 2.0, f64::from(height) + padding * 2.0);
    if width <= 0 || height <= 0 {
        tracing::warn!(width, height, "watermark text has no glyphs to draw");
        return false;
    }
    let snapshot = gtk::Snapshot::new();
    snapshot.translate(&graphene::Point::new(narrow(padding), narrow(padding)));
    snapshot.append_layout(&layout, color);
    let Some(node) = snapshot.to_node() else {
        return false;
    };
    let bounds = graphene::Rect::new(0.0, 0.0, narrow(canvas.0.ceil()), narrow(canvas.1.ceil()));
    let renderer = gsk::CairoRenderer::new();
    if let Err(err) = renderer.realize(None::<&gdk::Surface>) {
        tracing::warn!(%err, "watermark text renderer unavailable");
        return false;
    }
    let texture = renderer.render_texture(node, Some(&bounds));
    renderer.unrealize();
    match texture.save_to_png(file) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(%err, file = %file.display(), "watermark text PNG write failed");
            false
        }
    }
}

/// A canvas coordinate as graphene takes it; every value here is a few thousand pixels at most.
#[expect(
    clippy::cast_possible_truncation,
    reason = "canvas sizes are bounded by MAX_IMAGE_SIDE, far inside f32's exact integer range"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// Removes a text image no live terminal renders any more.
fn remove_unless_used(file: &Path) {
    let used = APPLIED.with(|applied| {
        applied
            .borrow()
            .values()
            .any(|entry| entry.text_file.as_deref() == Some(file))
    });
    if !used {
        remove_file(file);
    }
}

fn remove_file(file: &Path) {
    if let Err(err) = std::fs::remove_file(file)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%err, file = %file.display(), "could not remove a rendered watermark");
    }
}

/// The state directory, resolved once.
fn state_dir() -> &'static Path {
    static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();
    STATE_DIR.get_or_init(|| paths::state_dir(&ProcessEnv))
}

#[cfg(test)]
mod tests {
    use agterm_core::model::SessionSeed;
    use agterm_core::pane_env::uuid_string;

    use super::*;
    use crate::gtk_test::on_gtk_thread;

    fn watermark(kind: WatermarkKind, color_hex: Option<&str>) -> BackgroundWatermark {
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
        let mut session = Session::new(SessionSeed {
            initial_cwd: "/tmp".to_owned(),
            ..SessionSeed::default()
        });
        let foreground = Some(Color { r: 1, g: 2, b: 3 });
        assert_eq!(
            desired(&session, StatusPane::Left, foreground),
            Applied {
                watermark: None,
                text_file: None,
                foreground: None
            }
        );
        session.background_watermark = Some(watermark(WatermarkKind::Text, None));
        let inherited = desired(&session, StatusPane::Scratch, foreground);
        let id = uuid_string(session.id());
        assert_eq!(
            inherited.text_file,
            Some(state_dir().join(format!("watermarks/{id}.png")))
        );
        assert_eq!(inherited.foreground, foreground);

        session.pane_backgrounds.left = Some(watermark(WatermarkKind::Text, Some("#ff0000")));
        let own = desired(&session, StatusPane::Left, foreground);
        let key = uuid_string(session.pane_identity);
        assert_eq!(
            own.text_file,
            Some(state_dir().join(format!("watermarks/{id}-{key}.png")))
        );
        // a text with its own color does not follow the foreground
        assert_eq!(own.foreground, None);

        session.pane_backgrounds.scratch = Some(watermark(WatermarkKind::Color, Some("#102030")));
        let color = desired(&session, StatusPane::Scratch, foreground);
        assert_eq!((color.text_file, color.foreground), (None, None));
    }

    #[test]
    fn a_text_tint_is_its_color_else_the_foreground_else_white() {
        let gray = Color {
            r: 0x80,
            g: 0x80,
            b: 0x80,
        };
        assert_eq!(
            text_color(Some("#ff0000"), Some(gray)),
            gdk::RGBA::new(1.0, 0.0, 0.0, 1.0)
        );
        assert_eq!(text_color(Some("00ff00"), None), gdk::RGBA::new(0.0, 1.0, 0.0, 1.0));
        let half = 128.0 / 255.0;
        assert_eq!(text_color(None, Some(gray)), gdk::RGBA::new(half, half, half, 1.0));
        assert_eq!(text_color(Some("bogus"), None), gdk::RGBA::WHITE);
    }

    #[test]
    fn text_renders_to_a_png() {
        let rendered = on_gtk_thread(|| {
            let dir = tempfile::tempdir().expect("a temp dir");
            let file = dir.path().join("w.png");
            let drawn = render_text("PROD", &gdk::RGBA::WHITE, &file);
            let texture = gdk::Texture::from_filename(&file).expect("the PNG decodes");
            (drawn, texture.width(), texture.height())
        });
        if let Some((drawn, width, height)) = rendered {
            assert!(drawn);
            // the glyph run at 256 px plus 30% padding on each side
            assert!(width > height && height > 256, "{width}x{height}");
        }
    }

    #[test]
    fn a_long_text_is_drawn_smaller_to_stay_within_a_texture() {
        let rendered = on_gtk_thread(|| {
            let dir = tempfile::tempdir().expect("a temp dir");
            let file = dir.path().join("long.png");
            let drawn = render_text(&"W".repeat(watermark::MAX_TEXT_LENGTH), &gdk::RGBA::WHITE, &file);
            let texture = gdk::Texture::from_filename(&file).expect("the PNG decodes");
            (drawn, texture.width())
        });
        if let Some((drawn, width)) = rendered {
            assert!(drawn);
            assert!(width <= MAX_IMAGE_SIDE, "{width}");
        }
    }
}
