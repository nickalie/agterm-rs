//! Session backgrounds as libghostty config (upstream's `WatermarkConfig.swift` and `WatermarkStorage.swift`): the
//! validation the control server and the CLI share, the per-surface config overlay a [`BackgroundWatermark`] becomes,
//! and where rendered text watermarks live on disk. Host-free: the frontend rasterizes the text and pushes the
//! overlay to its terminals.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::model::{BackgroundWatermark, WatermarkFit, WatermarkKind, WatermarkPosition, is_valid_color_hex};
use crate::pane_env::uuid_string;

/// The longest watermark text, in characters: the frontend rasterizes it at a large fixed font size into a bitmap
/// sized to the glyph run, so an uncapped string would allocate without bound.
pub const MAX_TEXT_LENGTH: usize = 256;

/// The image formats libghostty reads for `background-image` (PNG or JPEG).
const SUPPORTED_IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// Whether `fit` is a `background-image-fit` value.
#[must_use]
pub fn is_valid_fit(fit: &str) -> bool {
    WatermarkFit::from_wire(fit).is_some()
}

/// Whether `position` is a `background-image-position` value.
#[must_use]
pub fn is_valid_position(position: &str) -> bool {
    WatermarkPosition::from_wire(position).is_some()
}

/// Whether `opacity` is a `background-image-opacity`: finite and within `0..=1`.
#[must_use]
pub fn is_valid_opacity(opacity: f64) -> bool {
    opacity.is_finite() && (0.0..=1.0).contains(&opacity)
}

/// Whether `text` is a watermark text: non-empty and at most [`MAX_TEXT_LENGTH`] characters.
#[must_use]
pub fn is_valid_text(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= MAX_TEXT_LENGTH
}

/// Whether `path` may go into a config line: no ASCII control character. The path is emitted raw, so a newline
/// would split the line and let its tail set another ghostty key in the surface's overlay.
#[must_use]
pub fn is_valid_image_path(path: &str) -> bool {
    !path.chars().any(|c| u32::from(c) < 0x20)
}

/// Whether `path` names an image format libghostty reads, by its extension (upstream's
/// `WatermarkRenderer.isSupportedImage`).
#[must_use]
pub fn is_supported_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SUPPORTED_IMAGE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

/// The per-surface ghostty config overlay (loaded last, so it wins) for `watermark`, pointed at `resolved_image_path`
/// (the user's file for an image, the rendered PNG for text, nothing for a color):
///
/// - a color: `background` plus `background-opacity` at the window opacity (clamped, 1 when not finite);
/// - an image or text: `background-opacity = 1` (image opacity is relative to it) and the `background-image*` lines;
/// - a `font-size` line keeping the terminal's zoom, which a per-surface config update would otherwise reset.
///
/// No watermark, an image or text without a path, or a free-text value that fails validation again (a hand-edited
/// snapshot reaches here unchecked) leave only the font-size line; nothing to override is `""`.
#[must_use]
pub fn overlay_text(
    watermark: Option<&BackgroundWatermark>,
    resolved_image_path: Option<&str>,
    font_size: Option<f64>,
    window_opacity: f64,
) -> String {
    let mut lines = Vec::new();
    match watermark {
        Some(watermark) if watermark.kind == WatermarkKind::Color => {
            if let Some(hex) = watermark.color_hex.as_deref().filter(|hex| is_valid_color_hex(hex)) {
                lines.push(format!("background = {hex}"));
                lines.push(format!(
                    "background-opacity = {}",
                    formatted(clamped_opacity(window_opacity))
                ));
            }
        }
        Some(watermark) => {
            if let Some(path) = resolved_image_path.filter(|path| is_valid_image_path(path)) {
                image_lines(watermark, path, &mut lines);
            }
        }
        None => {}
    }
    if let Some(size) = font_size {
        lines.push(format!("font-size = {}", formatted(size)));
    }
    joined(&lines)
}

fn image_lines(watermark: &BackgroundWatermark, path: &str, lines: &mut Vec<String>) {
    lines.push("background-opacity = 1".to_owned());
    lines.push(format!("background-image = {path}"));
    if let Some(opacity) = watermark.opacity {
        lines.push(format!("background-image-opacity = {}", formatted(opacity)));
    }
    let fit = watermark.fit.unwrap_or(WatermarkFit::Contain);
    let position = watermark.position.unwrap_or(WatermarkPosition::Center);
    lines.push(format!("background-image-fit = {}", fit.wire_name()));
    lines.push(format!("background-image-position = {}", position.wire_name()));
    lines.push(format!("background-image-repeat = {}", watermark.repeats == Some(true)));
}

/// The overlay that makes a program's live OSC 11 background visible under window translucency: only
/// `background-opacity` at the window opacity, plus the zoom-keeping `font-size`. Never a `background` key: that would
/// re-seed the terminal's default color, and the program's OSC 111 reset would then restore its own color.
#[must_use]
pub fn osc_background_overlay_text(font_size: Option<f64>, window_opacity: f64) -> String {
    let mut lines = vec![format!(
        "background-opacity = {}",
        formatted(clamped_opacity(window_opacity))
    )];
    if let Some(size) = font_size {
        lines.push(format!("font-size = {}", formatted(size)));
    }
    joined(&lines)
}

fn clamped_opacity(opacity: f64) -> f64 {
    if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn joined(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

/// A number as a ghostty config value: an integral one without a fraction (`14`), else the shortest fixed-point form
/// with up to six decimals. Never scientific notation, which ghostty's parser rejects; total for any `f64`.
#[must_use]
pub fn formatted(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.2e18 {
        // `+ 0.0` turns a negative zero into `0`
        return format!("{:.0}", value + 0.0);
    }
    let text = format!("{value:.6}");
    if !text.contains('.') {
        return text;
    }
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

// MARK: - storage

/// `<state dir>/watermarks`, where rendered text watermarks live; not created.
#[must_use]
pub fn directory(state_dir: &Path) -> PathBuf {
    state_dir.join("watermarks")
}

/// [`directory`], created when missing (best effort, as a failed render reports itself).
pub fn ensure_directory(state_dir: &Path) -> PathBuf {
    let directory = directory(state_dir);
    if let Err(err) = std::fs::create_dir_all(&directory) {
        tracing::warn!(%err, directory = %directory.display(), "could not create the watermarks directory");
    }
    directory
}

/// A rendered text watermark: `<SESSION>.png` for the session default, `<SESSION>-<pane key>.png` for a pane
/// override ([`crate::model::Session::background_file_key`]).
#[must_use]
pub fn rendered_text_path(state_dir: &Path, session: Uuid, pane_key: Option<&str>) -> PathBuf {
    let name = match pane_key {
        Some(key) => format!("{}-{key}.png", uuid_string(session)),
        None => format!("{}.png", uuid_string(session)),
    };
    directory(state_dir).join(name)
}

/// Deletes the default's or one pane override's rendered text; a missing file is fine.
pub fn remove_rendered_text(state_dir: &Path, session: Uuid, pane_key: Option<&str>) {
    remove_rendered_file(&rendered_text_path(state_dir, session, pane_key));
}

/// Deletes every rendered text a session owns, for its permanent removal.
pub fn remove_all_rendered_text(state_dir: &Path, session: Uuid) {
    let directory = directory(state_dir);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let id = uuid_string(session);
    let whole = format!("{id}.png");
    let prefix = format!("{id}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let png = Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"));
        if name == whole || (name.starts_with(&prefix) && png) {
            remove_rendered_file(&entry.path());
        }
    }
}

/// Deletes one rendered text file by its path, as the frontend tracks it; a missing file is fine, any other failure
/// is logged.
pub fn remove_rendered_file(path: &Path) {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!(%err, path = %path.display(), "could not remove a rendered watermark");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
