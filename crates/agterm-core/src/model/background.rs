//! A session's background (upstream's `BackgroundWatermark.swift`, `PaneBackgrounds.swift`): an image, a rasterized
//! text or a solid color behind the terminal grid, per session with per-pane overrides. Stored on the session and
//! persisted in its snapshot; the commands that set it and the ghostty overlay that draws it come with their tasks.

use serde::{Deserialize, Serialize};

use crate::persist::lossy;

/// What the background is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatermarkKind {
    /// A user-supplied image file.
    Image,
    /// A string agterm rasterizes to an image.
    Text,
    /// A solid `#rrggbb` background color.
    Color,
}

/// libghostty's `background-image-fit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatermarkFit {
    Contain,
    Cover,
    Stretch,
    None,
}

/// libghostty's `background-image-position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WatermarkPosition {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// One background, in upstream's persisted shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundWatermark {
    pub kind: WatermarkKind,
    /// `image`: the image file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
    /// `text`: the string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `text`: its color; `color`: the background color. `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<WatermarkFit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<WatermarkPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats: Option<bool>,
}

/// Per-pane overrides of a session's background; a `None` pane inherits it. Each pane decodes on its own, so an
/// unreadable override drops to inherit without costing the others.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PaneBackgrounds {
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub left: Option<BackgroundWatermark>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub right: Option<BackgroundWatermark>,
    #[serde(default, deserialize_with = "lossy", skip_serializing_if = "Option::is_none")]
    pub scratch: Option<BackgroundWatermark>,
}

impl PaneBackgrounds {
    /// Whether every pane inherits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none() && self.scratch.is_none()
    }

    /// The snapshot form: the left and right overrides only (the scratch's is never persisted), `None` when neither
    /// is set.
    #[must_use]
    pub fn persisted(&self) -> Option<Self> {
        let kept = Self {
            left: self.left.clone(),
            right: self.right.clone(),
            scratch: None,
        };
        (!kept.is_empty()).then_some(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watermark_round_trips_its_raw_names() {
        let text = r##"{"kind":"text","text":"PROD","colorHex":"#ff0000","fit":"cover","position":"bottom-right","repeats":true,"opacity":0.5}"##;
        let watermark: BackgroundWatermark = serde_json::from_str(text).unwrap();
        assert_eq!(watermark.kind, WatermarkKind::Text);
        assert_eq!(watermark.position, Some(WatermarkPosition::BottomRight));
        assert_eq!(watermark.fit, Some(WatermarkFit::Cover));
        let again: BackgroundWatermark = serde_json::from_value(serde_json::to_value(&watermark).unwrap()).unwrap();
        assert_eq!(again, watermark);
    }

    #[test]
    fn an_unreadable_pane_override_drops_alone() {
        let text = r##"{"left":{"kind":"sparkles"},"right":{"kind":"color","colorHex":"#000000"}}"##;
        let panes: PaneBackgrounds = serde_json::from_str(text).unwrap();
        assert_eq!(panes.left, None);
        assert_eq!(panes.right.map(|right| right.kind), Some(WatermarkKind::Color));
    }

    #[test]
    fn the_persisted_form_drops_the_scratch_override() {
        let color = BackgroundWatermark {
            kind: WatermarkKind::Color,
            image_path: None,
            text: None,
            color_hex: Some("#112233".to_owned()),
            opacity: None,
            fit: None,
            position: None,
            repeats: None,
        };
        let scratch_only = PaneBackgrounds {
            scratch: Some(color.clone()),
            ..PaneBackgrounds::default()
        };
        assert_eq!(scratch_only.persisted(), None);
        let both = PaneBackgrounds {
            left: Some(color.clone()),
            scratch: Some(color),
            ..PaneBackgrounds::default()
        };
        assert!(
            both.persisted()
                .is_some_and(|kept| kept.scratch.is_none() && kept.left.is_some())
        );
    }
}
