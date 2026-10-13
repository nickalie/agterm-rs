//! A session's background (upstream's `BackgroundWatermark.swift`, `PaneBackgrounds.swift` and
//! `AppStore.setBackgroundWatermark`): an image, a rasterized text or a solid color behind the terminal grid, per
//! session with per-pane overrides. Stored on the session and persisted in its snapshot; `session.background` sets it
//! and the frontend turns each pane's effective one into a per-surface config overlay ([`crate::watermark`]).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AppStore, Session, StatusPane};
use crate::pane_env::uuid_string;
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

impl WatermarkFit {
    /// Every value, in upstream's `CaseIterable` order.
    pub const ALL: [Self; 4] = [Self::Contain, Self::Cover, Self::Stretch, Self::None];

    /// The ghostty and wire spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Contain => "contain",
            Self::Cover => "cover",
            Self::Stretch => "stretch",
            Self::None => "none",
        }
    }

    /// Parses the ghostty spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|fit| fit.wire_name() == name)
    }
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

impl WatermarkPosition {
    /// Every value, in upstream's `CaseIterable` order.
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::TopCenter,
        Self::TopRight,
        Self::CenterLeft,
        Self::Center,
        Self::CenterRight,
        Self::BottomLeft,
        Self::BottomCenter,
        Self::BottomRight,
    ];

    /// The ghostty and wire spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopCenter => "top-center",
            Self::TopRight => "top-right",
            Self::CenterLeft => "center-left",
            Self::Center => "center",
            Self::CenterRight => "center-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomCenter => "bottom-center",
            Self::BottomRight => "bottom-right",
        }
    }

    /// Parses the ghostty spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|position| position.wire_name() == name)
    }
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
    /// The override of `pane`.
    #[must_use]
    pub fn get(&self, pane: StatusPane) -> Option<&BackgroundWatermark> {
        match pane {
            StatusPane::Left => self.left.as_ref(),
            StatusPane::Right => self.right.as_ref(),
            StatusPane::Scratch => self.scratch.as_ref(),
        }
    }

    /// The mutable override slot of `pane`.
    pub fn slot_mut(&mut self, pane: StatusPane) -> &mut Option<BackgroundWatermark> {
        match pane {
            StatusPane::Left => &mut self.left,
            StatusPane::Right => &mut self.right,
            StatusPane::Scratch => &mut self.scratch,
        }
    }

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

impl Session {
    /// What pane `pane` renders: its own override, else the session default.
    #[must_use]
    pub fn effective_background(&self, pane: StatusPane) -> Option<&BackgroundWatermark> {
        self.pane_backgrounds.get(pane).or(self.background_watermark.as_ref())
    }

    /// The solid color pane `pane` renders, which a wash over it blends toward; `None` for the theme background,
    /// including under an image or text.
    #[must_use]
    pub fn wash_color_hex(&self, pane: StatusPane) -> Option<&str> {
        self.effective_background(pane)
            .filter(|watermark| watermark.kind == WatermarkKind::Color)
            .and_then(|watermark| watermark.color_hex.as_deref())
    }

    /// The key naming a pane override's rendered text file: the pane identity, which follows the terminal across a
    /// swap and a promotion, or `scratch`; `None` for a right pane that does not exist.
    #[must_use]
    pub fn background_file_key(&self, pane: StatusPane) -> Option<String> {
        match pane {
            StatusPane::Left => Some(uuid_string(self.pane_identity)),
            StatusPane::Right => self.split_pane_identity.map(uuid_string),
            StatusPane::Scratch => Some("scratch".to_owned()),
        }
    }
}

impl AppStore {
    /// Writes the session default when `pane` is `None`, leaving the overrides, or that pane's override, where `None`
    /// returns the pane to inheriting; saves on a change. Whether it changed (false for an unknown session), so the
    /// caller re-applies only what moved. The rendered text files belong to the frontend, which draws them.
    pub fn set_background_watermark(
        &mut self,
        watermark: Option<BackgroundWatermark>,
        id: Uuid,
        pane: Option<StatusPane>,
    ) -> bool {
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        let slot = match pane {
            Some(pane) => session.pane_backgrounds.slot_mut(pane),
            None => &mut session.background_watermark,
        };
        if *slot == watermark {
            return false;
        }
        *slot = watermark;
        self.save();
        true
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

    fn text(text: &str) -> BackgroundWatermark {
        BackgroundWatermark {
            kind: WatermarkKind::Text,
            image_path: None,
            text: Some(text.to_owned()),
            color_hex: None,
            opacity: None,
            fit: None,
            position: None,
            repeats: None,
        }
    }

    #[test]
    fn each_pane_reads_and_writes_its_own_slot() {
        let mut backgrounds = PaneBackgrounds::default();
        for pane in [StatusPane::Left, StatusPane::Right, StatusPane::Scratch] {
            *backgrounds.slot_mut(pane) = Some(text("DRIVER"));
            assert_eq!(backgrounds.get(pane), Some(&text("DRIVER")));
            *backgrounds.slot_mut(pane) = None;
        }
        assert!(backgrounds.is_empty());
    }

    /// Upstream's `settingTheDefaultLeavesOverridesAndAPaneClearReturnsToInherit`.
    #[test]
    fn the_default_leaves_overrides_and_a_pane_clear_returns_to_inheriting() {
        let mut store = AppStore::new();
        let workspace = store.add_workspace("work");
        let id = store
            .add_session(workspace, crate::model::NewSession::at_cwd("/tmp"))
            .expect("a session");
        let (peer, tint) = (text("PEER"), text("TINT"));
        assert!(store.set_background_watermark(Some(peer.clone()), id, Some(StatusPane::Right)));
        assert!(!store.set_background_watermark(Some(peer.clone()), id, Some(StatusPane::Right)));
        assert!(store.set_background_watermark(Some(tint.clone()), id, None));
        let session = |store: &AppStore| store.session(id).cloned_backgrounds();
        assert_eq!(session(&store).1.right, Some(peer.clone()));
        assert!(store.set_background_watermark(None, id, None));
        assert_eq!(
            session(&store),
            (
                None,
                PaneBackgrounds {
                    right: Some(peer),
                    ..PaneBackgrounds::default()
                }
            )
        );
        assert!(store.set_background_watermark(Some(tint.clone()), id, None));
        assert!(store.set_background_watermark(None, id, Some(StatusPane::Right)));
        assert!(session(&store).1.is_empty());
        let effective = store
            .session(id)
            .and_then(|s| s.effective_background(StatusPane::Right).cloned());
        assert_eq!(effective, Some(tint));
        assert!(!store.set_background_watermark(None, Uuid::new_v4(), Some(StatusPane::Left)));
    }

    trait ClonedBackgrounds {
        fn cloned_backgrounds(self) -> (Option<BackgroundWatermark>, PaneBackgrounds);
    }

    impl ClonedBackgrounds for Option<&Session> {
        fn cloned_backgrounds(self) -> (Option<BackgroundWatermark>, PaneBackgrounds) {
            self.map(|s| (s.background_watermark.clone(), s.pane_backgrounds.clone()))
                .unwrap_or_default()
        }
    }
}
