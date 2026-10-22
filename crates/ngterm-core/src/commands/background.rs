//! `session.background`: a session's background default or one pane's override. Validation and its messages follow
//! `dispatchSessionBackground` in upstream's `ControlDispatcher.swift`; the effect follows `setSessionBackground` in
//! `agterm/Control/ControlServer+SurfaceIO.swift`. The frontend re-applies each pane's effective background to its
//! terminal on its next page sync, only where it changed, and owns the rendered text files.

use std::path::Path;

use ngterm_proto::{Args, Request};

use super::surface::{no_scratch, no_split_pane};
use super::{Outcome, args_of, parse_pane, session_store, with_id};
use crate::Core;
use crate::error::ControlError;
use crate::model::{
    BackgroundWatermark, StatusPane, WatermarkFit, WatermarkKind, WatermarkPosition, is_valid_color_hex,
};
use crate::watermark::{
    MAX_TEXT_LENGTH, is_supported_image, is_valid_fit, is_valid_image_path, is_valid_opacity, is_valid_position,
};

/// Answers `session.background`: the arguments checked in upstream's order, then the image file, then the target.
pub(crate) fn dispatch(core: &mut Core, request: &Request) -> Outcome {
    let args = args_of(request);
    let watermark = parse(args)?;
    let pane = parse_pane(args.pane.as_deref())?;
    if let Some(watermark) = &watermark {
        check_image_file(watermark)?;
    }
    let (store, id) = session_store(core, request.target.as_deref(), args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    // an override for a pane that does not exist would outlive nothing and land on the next one
    match pane {
        Some(StatusPane::Right) if !session.split.exists => return Err(no_split_pane()),
        Some(StatusPane::Scratch) if !session.scratch_surface.is_present() => return Err(no_scratch()),
        _ => {}
    }
    // unchanged is a no-op: no save, and the frontend re-applies nothing
    store.set_background_watermark(watermark, id, pane);
    Ok(with_id(id))
}

/// The background `args` ask for, `None` to clear: the shared fit, position and opacity checks first, then the mode's
/// own.
fn parse(args: &Args) -> Result<Option<BackgroundWatermark>, ControlError> {
    if let Some(fit) = args.fit.as_deref().filter(|fit| !is_valid_fit(fit)) {
        return Err(ControlError::rejected(format!(
            "invalid fit: {fit} (contain|cover|stretch|none)"
        )));
    }
    if let Some(position) = args.position.as_deref().filter(|position| !is_valid_position(position)) {
        return Err(ControlError::rejected(format!("invalid position: {position}")));
    }
    if let Some(opacity) = args.opacity.filter(|opacity| !is_valid_opacity(*opacity)) {
        return Err(ControlError::rejected(format!(
            "invalid opacity: {} (0.0-1.0)",
            swift_double(opacity)
        )));
    }
    match args.mode.as_deref() {
        Some("image") => image(args).map(Some),
        Some("text") => text(args).map(Some),
        Some("color") => color(args).map(Some),
        Some("clear") | None => Ok(None),
        Some(mode) => Err(ControlError::rejected(format!(
            "invalid background mode: {mode} (image|text|color|clear)"
        ))),
    }
}

fn image(args: &Args) -> Result<BackgroundWatermark, ControlError> {
    let path = args
        .path
        .as_deref()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| ControlError::rejected("session.background image requires a path"))?;
    if !is_valid_image_path(path) {
        return Err(ControlError::rejected("image path must not contain control characters"));
    }
    Ok(BackgroundWatermark {
        image_path: Some(path.to_owned()),
        repeats: args.repeats,
        ..placed(WatermarkKind::Image, args)
    })
}

fn text(args: &Args) -> Result<BackgroundWatermark, ControlError> {
    let text = args
        .text
        .as_deref()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| ControlError::rejected("session.background text requires text"))?;
    if text.chars().count() > MAX_TEXT_LENGTH {
        return Err(ControlError::rejected(format!(
            "session.background text too long (max {MAX_TEXT_LENGTH} characters)"
        )));
    }
    if let Some(color) = args.color.as_deref().filter(|color| !is_valid_color_hex(color)) {
        return Err(invalid_color(color));
    }
    Ok(BackgroundWatermark {
        text: Some(text.to_owned()),
        color_hex: args.color.clone(),
        ..placed(WatermarkKind::Text, args)
    })
}

/// A solid color takes no opacity of its own: it is drawn at the window's translucency.
fn color(args: &Args) -> Result<BackgroundWatermark, ControlError> {
    let color = args
        .color
        .as_deref()
        .filter(|color| !color.is_empty())
        .ok_or_else(|| ControlError::rejected("session.background color requires a color"))?;
    if !is_valid_color_hex(color) {
        return Err(invalid_color(color));
    }
    Ok(BackgroundWatermark {
        kind: WatermarkKind::Color,
        image_path: None,
        text: None,
        color_hex: Some(color.to_owned()),
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    })
}

/// An image or text background with the call's opacity, fit and position (already validated).
fn placed(kind: WatermarkKind, args: &Args) -> BackgroundWatermark {
    BackgroundWatermark {
        kind,
        image_path: None,
        text: None,
        color_hex: None,
        opacity: args.opacity,
        fit: args.fit.as_deref().and_then(WatermarkFit::from_wire),
        position: args.position.as_deref().and_then(WatermarkPosition::from_wire),
        repeats: None,
    }
}

fn invalid_color(color: &str) -> ControlError {
    ControlError::rejected(format!("invalid color: {color} (#rrggbb)"))
}

/// An image must be a PNG or JPEG file that exists, checked before the target resolves (upstream's app side).
fn check_image_file(watermark: &BackgroundWatermark) -> Result<(), ControlError> {
    let Some(path) = watermark
        .image_path
        .as_deref()
        .filter(|_| watermark.kind == WatermarkKind::Image)
    else {
        return Ok(());
    };
    if !is_supported_image(path) {
        return Err(ControlError::rejected(format!(
            "unsupported image (PNG or JPEG only): {path}"
        )));
    }
    if !Path::new(path).exists() {
        return Err(ControlError::rejected(format!("no such image file: {path}")));
    }
    Ok(())
}

/// A `Double` as Swift's string interpolation writes it, which upstream's message embeds: integral values keep `.0`
/// (`2.0`), others their shortest round-trip form; non-finite ones as `inf`, `-inf`, `nan`.
fn swift_double(value: f64) -> String {
    if value.is_nan() {
        "nan".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_owned()
    } else {
        // Rust's `Debug` is the shortest round-trip form with a `.0` on integral values, as Swift's `description`
        format!("{value:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacities_print_as_swift_writes_them() {
        assert_eq!(swift_double(2.0), "2.0");
        assert_eq!(swift_double(1.5), "1.5");
        assert_eq!(swift_double(-0.1), "-0.1");
        assert_eq!(swift_double(f64::NAN), "nan");
        assert_eq!(swift_double(f64::NEG_INFINITY), "-inf");
    }
}
