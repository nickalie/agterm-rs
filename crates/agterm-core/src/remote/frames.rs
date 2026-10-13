//! The presentation stream's frames (upstream's `PresentationFrames.swift` and `PresentationLayout.swift`): one JSON
//! object per line, `{"kind", "gen", "rev", <payload>}`, carried between an origin session and the viewers attached to
//! it.
//!
//! `gen` is the connection generation and `rev` a revision monotonic within it, so a receiver drops whatever is left
//! over from an earlier connection. A kind this build does not speak decodes to [`Body::Unknown`], keeping its place
//! in the order, so a newer peer does not break the stream.

use std::fmt;

use agterm_proto::AskButton;
use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::hud::{HudPosition, HudSpec, HudSpinner};
use crate::model::{AgentStatus, AskAlignment, AskStyle, PendingAsk, StatusShape};
use crate::pane_env::uuid_string;

/// The presentation protocol version this build speaks.
pub const VERSION: i64 = 1;
/// A HUD body is the largest legitimate frame; this leaves it room and still bounds a hostile line.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// Frames a subscriber may have queued before it counts as stalled.
pub const MAX_PENDING_FRAMES: usize = 256;

/// A UUID as Foundation reads one: the hyphenated form, either case.
pub(crate) fn parse_uuid(raw: &str) -> Option<Uuid> {
    (raw.len() == 36).then(|| Uuid::parse_str(raw).ok()).flatten()
}

/// A UUID as Foundation encodes one: hyphenated, upper case.
fn serialize_uuid<S: Serializer>(id: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&uuid_string(*id))
}

/// A pane of the origin session on the wire (upstream's `PresentationPane`). A main or split pane travels as its stable
/// identity, never as a left/right role, so a swap or promotion on either side cannot misroute it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresentationPane {
    Identity(Uuid),
    Scratch,
}

const SCRATCH: &str = "scratch";

impl Serialize for PresentationPane {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Identity(id) => serializer.serialize_str(&uuid_string(*id)),
            Self::Scratch => serializer.serialize_str(SCRATCH),
        }
    }
}

impl<'de> Deserialize<'de> for PresentationPane {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == SCRATCH {
            return Ok(Self::Scratch);
        }
        parse_uuid(&raw)
            .map(Self::Identity)
            .ok_or_else(|| de::Error::custom("pane is neither scratch nor a uuid"))
    }
}

/// What a viewer asks to be: a mirror beside the origin, or the session's sole presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresentationMode {
    Mirror,
    Presenter,
}

/// Opens a stream in both directions and carries what each side can speak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationHello {
    pub version: i64,
    pub kinds: Vec<String>,
    pub mode: PresentationMode,
}

/// Serde for a status by its wire spelling.
mod status_wire {
    use super::{AgentStatus, Deserialize, Deserializer, Serializer, de};

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` hands the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(status: &AgentStatus, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(status.wire_name())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<AgentStatus, D::Error> {
        let raw = String::deserialize(deserializer)?;
        AgentStatus::from_wire(&raw).ok_or_else(|| de::Error::custom(format!("unknown status: {raw}")))
    }
}

/// Serde for an optional shape by its wire spelling.
mod shape_wire {
    use super::{Deserialize, Deserializer, Serializer, StatusShape, de};

    #[expect(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` hands the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(shape: &Option<StatusShape>, serializer: S) -> Result<S::Ok, S::Error> {
        match shape {
            Some(shape) => serializer.serialize_some(shape.wire_name()),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<StatusShape>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|raw| StatusShape::from_wire(&raw).ok_or_else(|| de::Error::custom(format!("unknown shape: {raw}"))))
            .transpose()
    }
}

/// A session's full non-idle agent status; idle travels as an absent status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationStatus {
    #[serde(with = "status_wire")]
    pub status: AgentStatus,
    pub blink: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "shape_wire")]
    pub shape: Option<StatusShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PresentationPane>,
    /// Epoch seconds on the origin's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<f64>,
}

/// A HUD spec as it travels (upstream's `HudSpec` coding: every field optional on decode but the message).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HudWire {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spinner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size_percent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hide_after: Option<f64>,
    #[serde(default)]
    markdown: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size: Option<f64>,
    #[serde(default)]
    sticky: bool,
    #[serde(default = "default_frame")]
    frame: bool,
}

fn default_frame() -> bool {
    true
}

/// Serde for a [`HudSpec`] through [`HudWire`].
mod hud_spec {
    use super::{Deserialize, Deserializer, HudPosition, HudSpec, HudSpinner, HudWire, Serialize, Serializer, de};

    pub(super) fn serialize<S: Serializer>(spec: &HudSpec, serializer: S) -> Result<S::Ok, S::Error> {
        HudWire {
            message: spec.message.clone(),
            detail: spec.detail.clone(),
            spinner: spec.spinner.map(|spinner| spinner.wire_name().to_owned()),
            background_color: spec.background_color.clone(),
            text_color: spec.text_color.clone(),
            size_percent: spec.size_percent,
            position: Some(spec.position.wire_name().to_owned()),
            hide_after: spec.hide_after,
            markdown: spec.markdown,
            font_size: spec.font_size,
            sticky: spec.sticky,
            frame: spec.frame,
        }
        .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<HudSpec, D::Error> {
        let wire = HudWire::deserialize(deserializer)?;
        let spinner = wire
            .spinner
            .map(|raw| HudSpinner::from_wire(&raw).ok_or_else(|| de::Error::custom(format!("unknown spinner: {raw}"))))
            .transpose()?;
        let position = match wire.position {
            Some(raw) => {
                HudPosition::parse(&raw).ok_or_else(|| de::Error::custom(format!("unknown hud position: {raw}")))?
            }
            None => HudPosition::DEFAULT,
        };
        Ok(HudSpec {
            message: wire.message,
            detail: wire.detail,
            spinner,
            background_color: wire.background_color,
            text_color: wire.text_color,
            size_percent: wire.size_percent,
            position,
            hide_after: wire.hide_after,
            markdown: wire.markdown,
            font_size: wire.font_size,
            sticky: wire.sticky,
            frame: wire.frame,
        })
    }
}

/// The origin's live HUD. `generation` counts its publications; a withdrawal carries none, and the frame's `rev`
/// orders it against a replacement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresentationHud {
    #[serde(with = "hud_spec")]
    pub spec: HudSpec,
    /// The pane a pane-scoped panel sits over; `None` for a session-wide one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PresentationPane>,
    pub generation: i64,
    /// Seconds until the origin hides the panel, sampled when the frame was built; `None` for a persistent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
}

/// One control-origin notification: a live event, never part of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationNotify {
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PresentationPane>,
    pub source: String,
}

/// Serde for an ask style by its wire spelling.
mod style_wire {
    use super::{AskStyle, Deserialize, Deserializer, Serializer, de};

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` hands the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(style: &AskStyle, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match style {
            AskStyle::Terminal => "terminal",
            AskStyle::Gui => "gui",
        })
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<AskStyle, D::Error> {
        let raw = String::deserialize(deserializer)?;
        AskStyle::from_wire(&raw).ok_or_else(|| de::Error::custom(format!("unknown style: {raw}")))
    }
}

/// Serde for an ask alignment by its wire spelling.
mod align_wire {
    use super::{AskAlignment, Deserialize, Deserializer, Serializer, de};

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` hands the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(align: &AskAlignment, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match align {
            AskAlignment::Left => "left",
            AskAlignment::Center => "center",
            AskAlignment::Right => "right",
        })
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<AskAlignment, D::Error> {
        let raw = String::deserialize(deserializer)?;
        AskAlignment::from_wire(&raw).ok_or_else(|| de::Error::custom(format!("unknown align: {raw}")))
    }
}

/// An ask the origin hands its presenter to draw. `owner` is the presenter generation it was handed under, and every
/// answer echoes it, so one arriving after a handback is recognisably stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationAsk {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub buttons: Vec<AskButton>,
    #[serde(default, rename = "defaultID", skip_serializing_if = "Option::is_none")]
    pub default_id: Option<String>,
    #[serde(default, rename = "destructiveID", skip_serializing_if = "Option::is_none")]
    pub destructive_id: Option<String>,
    #[serde(with = "style_wire")]
    pub style: AskStyle,
    #[serde(with = "align_wire")]
    pub align: AskAlignment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    /// The origin's pane the dialog covers; `None` for the whole session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PresentationPane>,
    pub owner: i64,
}

impl PresentationAsk {
    /// `ask` as handed to the presenter of generation `owner`.
    #[must_use]
    pub fn new(ask: &PendingAsk, pane: Option<PresentationPane>, owner: i64) -> Self {
        Self {
            id: ask.id.clone(),
            title: ask.title.clone(),
            message: ask.message.clone(),
            buttons: ask.buttons.clone(),
            default_id: ask.default_id.clone(),
            destructive_id: ask.destructive_id.clone(),
            style: ask.style,
            align: ask.align,
            width: ask.width,
            pane,
            owner,
        }
    }
}

/// The presenter's answer: a button id, or `None` for a dismissal. Only the id travels; the origin derives the label
/// and index from the buttons it stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationAskAnswer {
    pub id: String,
    pub owner: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<String>,
}

/// Names one handed-over ask, for a refusal or a dismissal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationAskRef {
    pub id: String,
    pub owner: i64,
}

/// Asks the presenter to show an overlay whose program runs on the origin: the viewer opens a surface running the
/// job's helper over ssh. Nothing of the command or its environment travels; the helper gets those when it claims the
/// job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationOverlay {
    pub job: String,
    /// The origin's pane the overlay covers; `None` for the session-wide slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PresentationPane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_percent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    pub follow: bool,
    pub wait: bool,
}

/// Names one remote overlay job, with a size for a resize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationOverlayChange {
    pub job: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_percent: Option<i64>,
}

impl PresentationOverlayChange {
    #[must_use]
    pub fn job(job: impl Into<String>) -> Self {
        Self {
            job: job.into(),
            size_percent: None,
        }
    }
}

/// The origin's complete pane membership and arrangement, independent of surface realization (upstream's
/// `PresentationLayout`). It decodes lossily: a bad layout reads as [`PresentationLayout::invalid`] rather than
/// discarding the rest of a snapshot or disconnecting its stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresentationLayout {
    #[serde(serialize_with = "serialize_uuids")]
    pub panes: Vec<Uuid>,
    #[serde(serialize_with = "serialize_uuid")]
    pub primary: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis: Option<String>,
    pub shown: bool,
}

fn serialize_uuids<S: Serializer>(ids: &[Uuid], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(ids.iter().map(|id| uuid_string(*id)))
}

impl PresentationLayout {
    /// What an undecodable layout reads as: no panes, so it is never valid.
    #[must_use]
    pub fn invalid() -> Self {
        Self {
            panes: Vec::new(),
            primary: Uuid::nil(),
            axis: None,
            shown: false,
        }
    }

    /// One or two distinct panes including the primary; a single pane is never shown split, two need a known axis.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let distinct = self.panes.len() != 2 || self.panes[0] != self.panes[1];
        (1..=2).contains(&self.panes.len())
            && distinct
            && self.panes.contains(&self.primary)
            && if self.panes.len() == 1 {
                !self.shown
            } else {
                self.axis
                    .as_deref()
                    .is_some_and(|axis| crate::model::SplitAxis::from_wire(axis).is_some())
            }
    }

    /// The layout `value` holds, or [`PresentationLayout::invalid`].
    fn lossy(value: &Value) -> Self {
        Self::decoded(value).unwrap_or_else(Self::invalid)
    }

    fn decoded(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let panes = object
            .get("panes")?
            .as_array()?
            .iter()
            .map(|pane| pane.as_str().and_then(parse_uuid))
            .collect::<Option<Vec<_>>>()?;
        let primary = object.get("primary")?.as_str().and_then(parse_uuid)?;
        let axis = match object.get("axis") {
            None | Some(Value::Null) => None,
            Some(axis) => Some(axis.as_str()?.to_owned()),
        };
        let shown = object.get("shown")?.as_bool()?;
        Some(Self {
            panes,
            primary,
            axis,
            shown,
        })
    }
}

impl<'de> Deserialize<'de> for PresentationLayout {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::lossy(&Value::deserialize(deserializer)?))
    }
}

/// The replaceable state a subscriber starts from.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresentationSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PresentationStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hud: Option<PresentationHud>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<PresentationLayout>,
}

/// What one frame carries (upstream's `PresentationFrame.Body`).
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Hello(PresentationHello),
    Ping,
    Ack,
    Snapshot(PresentationSnapshot),
    Status(Option<PresentationStatus>),
    Context(Option<String>),
    Layout(PresentationLayout),
    Hud(Option<PresentationHud>),
    Notify(PresentationNotify),
    /// A viewer asking to be the session's sole presenter; sent only after the origin's hello offered it.
    PresenterAcquire,
    PresenterGranted,
    /// The role is held by another viewer; this one stays a mirror until it reconnects.
    PresenterRefused,
    AskRequest(PresentationAsk),
    AskResolve(PresentationAskAnswer),
    /// The presenter cannot show the ask, so the origin takes it back.
    AskRejected(PresentationAskRef),
    /// The ask ended on the origin; the presenter takes its dialog down without answering.
    AskDismiss(PresentationAskRef),
    OverlayRequest(PresentationOverlay),
    /// The presenter cannot show the overlay; the origin fails the job without launching anything.
    OverlayRejected(PresentationOverlayChange),
    OverlayClose(PresentationOverlayChange),
    OverlayResize(PresentationOverlayChange),
    /// The viewer closed its surface, which frees a slot held only for a `--wait` surface.
    OverlayClosed(PresentationOverlayChange),
    /// A kind this build does not speak, kept with its place in the order.
    Unknown(String),
}

impl Body {
    /// The frame's `kind`.
    #[must_use]
    pub fn kind(&self) -> &str {
        match self {
            Self::Hello(_) => "hello",
            Self::Ping => "ping",
            Self::Ack => "ack",
            Self::Snapshot(_) => "snapshot",
            Self::Status(_) => "status",
            Self::Context(_) => "context",
            Self::Layout(_) => "layout",
            Self::Hud(_) => "hud",
            Self::Notify(_) => "notify",
            Self::PresenterAcquire => "presenter.acquire",
            Self::PresenterGranted => "presenter.granted",
            Self::PresenterRefused => "presenter.refused",
            Self::AskRequest(_) => "ask.request",
            Self::AskResolve(_) => "ask.resolve",
            Self::AskRejected(_) => "ask.rejected",
            Self::AskDismiss(_) => "ask.dismiss",
            Self::OverlayRequest(_) => "overlay.request",
            Self::OverlayRejected(_) => "overlay.rejected",
            Self::OverlayClose(_) => "overlay.close",
            Self::OverlayResize(_) => "overlay.resize",
            Self::OverlayClosed(_) => "overlay.closed",
            Self::Unknown(kind) => kind,
        }
    }

    /// The payload's key and value, `None` for a kind without one (or an absent optional payload).
    fn payload(&self) -> serde_json::Result<Option<(&'static str, Value)>> {
        let entry = |key: &'static str, value: serde_json::Result<Value>| value.map(|value| Some((key, value)));
        match self {
            Self::Hello(hello) => entry("hello", serde_json::to_value(hello)),
            Self::Snapshot(snapshot) => entry("snapshot", serde_json::to_value(snapshot)),
            Self::Status(Some(status)) => entry("status", serde_json::to_value(status)),
            Self::Context(Some(context)) => entry("context", serde_json::to_value(context)),
            Self::Layout(layout) => entry("layout", serde_json::to_value(layout)),
            Self::Hud(Some(hud)) => entry("hud", serde_json::to_value(hud)),
            Self::Notify(notify) => entry("notify", serde_json::to_value(notify)),
            Self::AskRequest(ask) => entry("ask", serde_json::to_value(ask)),
            Self::AskResolve(answer) => entry("ask", serde_json::to_value(answer)),
            Self::AskRejected(reference) | Self::AskDismiss(reference) => entry("ask", serde_json::to_value(reference)),
            Self::OverlayRequest(overlay) => entry("overlay", serde_json::to_value(overlay)),
            Self::OverlayRejected(change)
            | Self::OverlayClose(change)
            | Self::OverlayResize(change)
            | Self::OverlayClosed(change) => entry("overlay", serde_json::to_value(change)),
            Self::Status(None)
            | Self::Context(None)
            | Self::Hud(None)
            | Self::Ping
            | Self::Ack
            | Self::PresenterAcquire
            | Self::PresenterGranted
            | Self::PresenterRefused
            | Self::Unknown(_) => Ok(None),
        }
    }
}

/// One line of the presentation stream.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentationFrame {
    pub generation: i64,
    pub rev: i64,
    pub body: Body,
}

/// Why a line could not become a frame, or a frame a line (upstream's `PresentationCodec.FrameError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The line or the encoded frame is this many bytes, over [`MAX_FRAME_BYTES`].
    Oversize(usize),
    /// What was wrong with it.
    Malformed(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversize(bytes) => write!(f, "frame of {bytes} bytes is over the limit"),
            Self::Malformed(detail) => write!(f, "malformed frame: {detail}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encodes `frame` as one line, its newline included.
///
/// # Errors
///
/// The frame cannot be encoded, or its line would reach [`MAX_FRAME_BYTES`].
pub fn encode(frame: &PresentationFrame) -> Result<Vec<u8>, FrameError> {
    let malformed = |error: serde_json::Error| FrameError::Malformed(error.to_string());
    let mut object = Map::new();
    object.insert("kind".to_owned(), Value::from(frame.body.kind()));
    object.insert("gen".to_owned(), Value::from(frame.generation));
    object.insert("rev".to_owned(), Value::from(frame.rev));
    if let Some((key, value)) = frame.body.payload().map_err(malformed)? {
        object.insert(key.to_owned(), value);
    }
    let mut line = serde_json::to_vec(&Value::Object(object)).map_err(malformed)?;
    if line.len() >= MAX_FRAME_BYTES {
        return Err(FrameError::Oversize(line.len()));
    }
    line.push(b'\n');
    Ok(line)
}

/// Decodes one line, without its newline.
///
/// # Errors
///
/// The line is over [`MAX_FRAME_BYTES`], or not a frame: what was wrong rides [`FrameError::Malformed`].
pub fn decode(line: &[u8]) -> Result<PresentationFrame, FrameError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversize(line.len()));
    }
    let value: Value = serde_json::from_slice(line).map_err(|error| FrameError::Malformed(error.to_string()))?;
    let Value::Object(mut object) = value else {
        return Err(FrameError::Malformed("a frame is a JSON object".to_owned()));
    };
    let generation = integer(&object, "gen")?;
    let rev = integer(&object, "rev")?;
    let kind = match object.get("kind") {
        Some(Value::String(kind)) => kind.clone(),
        Some(_) => return Err(FrameError::Malformed("kind: expected a string".to_owned())),
        None => return Err(FrameError::Malformed("missing kind".to_owned())),
    };
    let body = body(&kind, &mut object)?;
    Ok(PresentationFrame { generation, rev, body })
}

fn integer(object: &Map<String, Value>, key: &str) -> Result<i64, FrameError> {
    match object.get(key) {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| FrameError::Malformed(format!("{key}: expected an integer"))),
        None => Err(FrameError::Malformed(format!("missing {key}"))),
    }
}

/// The payload under `key`, which must be there.
fn required<T: DeserializeOwned>(object: &mut Map<String, Value>, key: &str) -> Result<T, FrameError> {
    let value = object
        .remove(key)
        .ok_or_else(|| FrameError::Malformed(format!("missing {key}")))?;
    serde_json::from_value(value).map_err(|error| FrameError::Malformed(format!("{key}: {error}")))
}

/// The payload under `key`, absent or `null` reading as `None`.
fn optional<T: DeserializeOwned>(object: &mut Map<String, Value>, key: &str) -> Result<Option<T>, FrameError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|error| FrameError::Malformed(format!("{key}: {error}"))),
    }
}

fn body(kind: &str, object: &mut Map<String, Value>) -> Result<Body, FrameError> {
    Ok(match kind {
        "hello" => Body::Hello(required(object, "hello")?),
        "ping" => Body::Ping,
        "ack" => Body::Ack,
        "snapshot" => Body::Snapshot(required(object, "snapshot")?),
        "status" => Body::Status(optional(object, "status")?),
        "context" => Body::Context(optional(object, "context")?),
        "layout" => Body::Layout(
            object
                .get("layout")
                .map_or_else(PresentationLayout::invalid, PresentationLayout::lossy),
        ),
        "hud" => Body::Hud(optional(object, "hud")?),
        "notify" => Body::Notify(required(object, "notify")?),
        "presenter.acquire" => Body::PresenterAcquire,
        "presenter.granted" => Body::PresenterGranted,
        "presenter.refused" => Body::PresenterRefused,
        "ask.request" => Body::AskRequest(required(object, "ask")?),
        "ask.resolve" => Body::AskResolve(required(object, "ask")?),
        "ask.rejected" => Body::AskRejected(required(object, "ask")?),
        "ask.dismiss" => Body::AskDismiss(required(object, "ask")?),
        "overlay.request" => Body::OverlayRequest(required(object, "overlay")?),
        "overlay.rejected" => Body::OverlayRejected(required(object, "overlay")?),
        "overlay.close" => Body::OverlayClose(required(object, "overlay")?),
        "overlay.resize" => Body::OverlayResize(required(object, "overlay")?),
        "overlay.closed" => Body::OverlayClosed(required(object, "overlay")?),
        other => Body::Unknown(other.to_owned()),
    })
}

/// The version both sides speak, or `None` when the peer's is not a version at all.
#[must_use]
pub fn negotiated_version(ours: i64, theirs: i64) -> Option<i64> {
    (ours >= 1 && theirs >= 1).then(|| ours.min(theirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> PresentationPane {
        PresentationPane::Identity(Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap())
    }

    fn status() -> PresentationStatus {
        PresentationStatus {
            status: AgentStatus::Blocked,
            blink: true,
            color: Some("#ff8800".to_owned()),
            shape: Some(StatusShape::Diamond),
            pane: Some(pane()),
            changed_at: Some(1_789_000_000.0),
        }
    }

    fn hud() -> PresentationHud {
        let mut spec = HudSpec::new("deploying");
        spec.hide_after = Some(10.0);
        PresentationHud {
            spec,
            pane: None,
            generation: 4,
            remaining: Some(7.5),
        }
    }

    fn pane_hud() -> PresentationHud {
        PresentationHud {
            spec: HudSpec::new("deploying"),
            pane: Some(pane()),
            generation: 5,
            remaining: None,
        }
    }

    fn ask() -> PendingAsk {
        PendingAsk {
            id: "a1".to_owned(),
            title: "deploy?".to_owned(),
            message: Some("to prod".to_owned()),
            buttons: vec![AskButton {
                id: "y".to_owned(),
                label: "Yes".to_owned(),
                hotkey: Some("y".to_owned()),
            }],
            default_id: Some("y".to_owned()),
            destructive_id: None,
            style: AskStyle::Gui,
            align: AskAlignment::Center,
            width: Some(40),
            anchor: None,
        }
    }

    fn change(size: Option<i64>) -> PresentationOverlayChange {
        PresentationOverlayChange {
            job: "j1".to_owned(),
            size_percent: size,
        }
    }

    fn reference() -> PresentationAskRef {
        PresentationAskRef {
            id: "a1".to_owned(),
            owner: 3,
        }
    }

    /// Upstream's `PresentationFramesTests.frames`.
    fn bodies() -> Vec<Body> {
        let mut bodies = state_bodies();
        bodies.extend(work_bodies());
        bodies
    }

    /// The hello, keepalive and presentation-state frames.
    fn state_bodies() -> Vec<Body> {
        vec![
            Body::Hello(PresentationHello {
                version: 1,
                kinds: vec!["status".to_owned(), "hud".to_owned(), "notify".to_owned()],
                mode: PresentationMode::Mirror,
            }),
            Body::Ping,
            Body::Ack,
            Body::Snapshot(PresentationSnapshot {
                status: Some(status()),
                hud: Some(hud()),
                ..PresentationSnapshot::default()
            }),
            Body::Snapshot(PresentationSnapshot::default()),
            Body::Status(Some(status())),
            Body::Status(None),
            Body::Status(Some(PresentationStatus {
                status: AgentStatus::Active,
                blink: false,
                color: None,
                shape: None,
                pane: Some(PresentationPane::Scratch),
                changed_at: None,
            })),
            Body::Hud(Some(hud())),
            Body::Hud(None),
            Body::Hud(Some(pane_hud())),
            Body::Snapshot(PresentationSnapshot {
                hud: Some(pane_hud()),
                ..PresentationSnapshot::default()
            }),
            Body::Notify(PresentationNotify {
                title: "build".to_owned(),
                body: "done".to_owned(),
                pane: Some(pane()),
                source: "control".to_owned(),
            }),
        ]
    }

    /// The presenter's frames and the work handed to it.
    fn work_bodies() -> Vec<Body> {
        vec![
            Body::Hello(PresentationHello {
                version: 1,
                kinds: vec!["status".to_owned()],
                mode: PresentationMode::Presenter,
            }),
            Body::PresenterAcquire,
            Body::PresenterGranted,
            Body::PresenterRefused,
            Body::AskRequest(PresentationAsk::new(&ask(), Some(pane()), 3)),
            Body::AskResolve(PresentationAskAnswer {
                id: "a1".to_owned(),
                owner: 3,
                button: Some("y".to_owned()),
            }),
            Body::AskResolve(PresentationAskAnswer {
                id: "a1".to_owned(),
                owner: 3,
                button: None,
            }),
            Body::AskRejected(reference()),
            Body::AskDismiss(reference()),
            Body::OverlayRequest(PresentationOverlay {
                job: "j1".to_owned(),
                pane: Some(pane()),
                size_percent: Some(60),
                background_color: Some("#102030".to_owned()),
                follow: true,
                wait: true,
            }),
            Body::OverlayRequest(PresentationOverlay {
                job: "j1".to_owned(),
                pane: None,
                size_percent: None,
                background_color: None,
                follow: false,
                wait: false,
            }),
            Body::OverlayRejected(change(None)),
            Body::OverlayClose(change(None)),
            Body::OverlayResize(change(Some(40))),
            Body::OverlayClosed(change(None)),
            Body::Context(Some("PR #517".to_owned())),
            Body::Context(None),
            Body::Snapshot(PresentationSnapshot {
                context: Some("PR #517".to_owned()),
                ..PresentationSnapshot::default()
            }),
            Body::Layout(PresentationLayout {
                panes: vec![Uuid::new_v4(), Uuid::new_v4()],
                primary: Uuid::nil(),
                axis: Some("vertical".to_owned()),
                shown: true,
            }),
        ]
    }

    #[test]
    fn every_frame_survives_a_round_trip() {
        for (rev, body) in (0..).zip(bodies()) {
            let frame = PresentationFrame {
                generation: 1,
                rev,
                body,
            };
            let line = encode(&frame).unwrap();
            assert_eq!(line.last(), Some(&b'\n'));
            assert!(!line[..line.len() - 1].contains(&b'\n'));
            assert_eq!(decode(&line[..line.len() - 1]).unwrap(), frame, "{}", frame.body.kind());
        }
    }

    #[test]
    fn the_wire_spells_ids_and_keys_as_upstream() {
        let frame = PresentationFrame {
            generation: 2,
            rev: 7,
            body: Body::AskRequest(PresentationAsk::new(&ask(), Some(pane()), 3)),
        };
        let line = encode(&frame).unwrap();
        let value: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["kind"], "ask.request");
        assert_eq!(value["ask"]["pane"], "11111111-2222-3333-4444-555555555555");
        assert_eq!(value["ask"]["defaultID"], "y");
        assert_eq!(value["ask"]["style"], "gui");
        assert!(value["ask"].get("destructiveID").is_none());
        let none = encode(&PresentationFrame {
            generation: 1,
            rev: 1,
            body: Body::Status(None),
        })
        .unwrap();
        assert_eq!(none, b"{\"kind\":\"status\",\"gen\":1,\"rev\":1}\n");
    }

    #[test]
    fn an_unknown_kind_decodes_to_unknown_and_keeps_its_ordering() {
        let line = br#"{"kind":"future.kind","gen":3,"rev":12,"job":"abc"}"#;
        assert_eq!(
            decode(line).unwrap(),
            PresentationFrame {
                generation: 3,
                rev: 12,
                body: Body::Unknown("future.kind".to_owned())
            }
        );
    }

    #[test]
    fn a_snapshot_from_an_origin_without_context_decodes_with_none() {
        let line = br#"{"kind":"snapshot","gen":1,"rev":1,"snapshot":{}}"#;
        assert_eq!(
            decode(line).unwrap().body,
            Body::Snapshot(PresentationSnapshot::default())
        );
    }

    #[test]
    fn an_oversize_line_is_refused_before_decoding() {
        let line = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert_eq!(decode(&line), Err(FrameError::Oversize(line.len())));
    }

    #[test]
    fn a_markdown_hud_with_its_font_size_survives_a_frame() {
        let mut spec = HudSpec::new("# Tasks\n\n- build");
        spec.markdown = true;
        spec.font_size = Some(18.0);
        let frame = PresentationFrame {
            generation: 1,
            rev: 3,
            body: Body::Hud(Some(PresentationHud {
                spec,
                pane: None,
                generation: 2,
                remaining: None,
            })),
        };
        let line = encode(&frame).unwrap();
        assert_eq!(decode(&line[..line.len() - 1]).unwrap(), frame);
    }

    #[test]
    fn the_largest_markdown_hud_fits_a_frame() {
        let mut spec = HudSpec::new("\u{1F600}".repeat(HudSpec::MAX_MARKDOWN_LENGTH));
        spec.detail = Some("\u{1F600}".repeat(HudSpec::MAX_TEXT_LENGTH));
        spec.markdown = true;
        let line = encode(&PresentationFrame {
            generation: 1,
            rev: 1,
            body: Body::Hud(Some(PresentationHud {
                spec,
                pane: None,
                generation: 1,
                remaining: None,
            })),
        })
        .unwrap();
        assert!(line.len() <= MAX_FRAME_BYTES);
    }

    #[test]
    fn a_frame_that_would_encode_oversize_is_refused() {
        let frame = PresentationFrame {
            generation: 1,
            rev: 1,
            body: Body::Notify(PresentationNotify {
                title: "t".to_owned(),
                body: "x".repeat(MAX_FRAME_BYTES),
                pane: None,
                source: "control".to_owned(),
            }),
        };
        assert!(matches!(encode(&frame), Err(FrameError::Oversize(_))));
    }

    #[test]
    fn a_malformed_line_reports_what_was_wrong() {
        for text in [
            "not json",
            r#"{"gen":1,"rev":1}"#,
            r#"{"kind":"status","rev":1}"#,
            r#"{"kind":"hello","gen":1,"rev":1}"#,
        ] {
            match decode(text.as_bytes()) {
                Err(FrameError::Malformed(detail)) => assert!(!detail.is_empty(), "{text}"),
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[test]
    fn negotiation_picks_the_lower_version() {
        for (ours, theirs, expected) in [(1, 1, 1), (1, 3, 1), (3, 1, 1), (2, 5, 2)] {
            assert_eq!(negotiated_version(ours, theirs), Some(expected));
        }
        for theirs in [0, -1] {
            assert_eq!(negotiated_version(1, theirs), None);
        }
    }

    /// Upstream's `PresentationLayoutTests`.
    #[test]
    fn layout_validity_and_lossy_decoding() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let single = PresentationLayout {
            panes: vec![a],
            primary: a,
            axis: None,
            shown: false,
        };
        assert!(single.is_valid());
        assert!(
            !PresentationLayout {
                shown: true,
                ..single.clone()
            }
            .is_valid()
        );
        let split = PresentationLayout {
            panes: vec![a, b],
            primary: a,
            axis: Some("horizontal".to_owned()),
            shown: false,
        };
        assert!(split.is_valid());
        for broken in [
            PresentationLayout {
                axis: None,
                ..split.clone()
            },
            PresentationLayout {
                axis: Some("diagonal".to_owned()),
                ..split.clone()
            },
            PresentationLayout {
                panes: vec![a, a],
                ..split.clone()
            },
            PresentationLayout {
                primary: Uuid::new_v4(),
                ..split.clone()
            },
            PresentationLayout::invalid(),
        ] {
            assert!(!broken.is_valid(), "{broken:?}");
        }
        let line = br#"{"kind":"snapshot","gen":1,"rev":1,"snapshot":{"context":"c","layout":{"panes":"no"}}}"#;
        let Body::Snapshot(snapshot) = decode(line).unwrap().body else {
            panic!("not a snapshot");
        };
        assert_eq!(snapshot.context.as_deref(), Some("c"), "a bad layout keeps the rest");
        assert_eq!(snapshot.layout, Some(PresentationLayout::invalid()));
        let bare = br#"{"kind":"layout","gen":1,"rev":1}"#;
        assert_eq!(decode(bare).unwrap().body, Body::Layout(PresentationLayout::invalid()));
        let older = br#"{"kind":"snapshot","gen":1,"rev":1,"snapshot":{"status":null}}"#;
        assert_eq!(
            decode(older).unwrap().body,
            Body::Snapshot(PresentationSnapshot::default())
        );
    }
}
