//! The agent-status glyph's vocabulary around [`AgentStatus`](super::AgentStatus) and the store's control-channel
//! status writes (upstream's `AgentStatus.swift` and `AppStore+Status.swift`, plus the sound names of the app's
//! `StatusSoundPlayer.swift` and the color check of `WatermarkConfig.isValidColorHex`).

use uuid::Uuid;

use super::pane::StatusPane;
use super::session::{AgentIndicator, AgentStatus};
use super::store::AppStore;
use crate::interrupt;

/// The silhouette a status glyph draws, per status in the settings or per call with `session.status --shape`;
/// `circle` is what an unset one draws (upstream's `StatusShape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusShape {
    Circle,
    Square,
    Triangle,
    Diamond,
    Capsule,
    Star,
}

impl StatusShape {
    /// Every shape, in upstream's declaration order (the order of the accepted-names lists).
    pub const ALL: [Self; 6] = [
        Self::Circle,
        Self::Square,
        Self::Triangle,
        Self::Diamond,
        Self::Capsule,
        Self::Star,
    ];

    /// The wire, settings and read-back spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Square => "square",
            Self::Triangle => "triangle",
            Self::Diamond => "diamond",
            Self::Capsule => "capsule",
            Self::Star => "star",
        }
    }

    /// Parses the wire spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shape| shape.wire_name() == name)
    }

    /// The accepted names pipe-joined, as the control server's rejection names them (upstream's `validNamesList`).
    #[must_use]
    pub fn valid_names_list() -> String {
        Self::ALL.map(Self::wire_name).join("|")
    }
}

/// Which keystroke clears a `blocked` or `completed` glyph (upstream's `StatusReset`, raw-stored in the settings'
/// `statusReset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum StatusReset {
    /// Any key.
    #[default]
    FirstKey,
    /// A submit (Return) only.
    Enter,
    /// None.
    Never,
}

impl StatusReset {
    /// The settings spelling.
    #[must_use]
    pub fn raw(self) -> &'static str {
        match self {
            Self::FirstKey => "firstKey",
            Self::Enter => "enter",
            Self::Never => "never",
        }
    }

    /// Parses the settings spelling; `None` for anything else.
    #[must_use]
    pub fn from_raw(raw: &str) -> Option<Self> {
        [Self::FirstKey, Self::Enter, Self::Never]
            .into_iter()
            .find(|reset| reset.raw() == raw)
    }
}

/// What one keystroke means to a status glyph (upstream's `StatusKeystroke`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusKeystroke {
    /// Escape or a bare Ctrl-C.
    Interrupt,
    /// Return with no modifier, or injected text carrying a newline.
    Submit,
    /// Plain typing.
    Other,
}

/// The outcome of a control-channel status write (upstream's `StatusWriteResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusWriteResult {
    Applied,
    /// Dropped whole: no status change, no restamp, no event, because `owner` holds a blocked status the writing
    /// pane may not replace.
    Refused {
        owner: StatusPane,
    },
}

/// The standard sound names `session.status --sound` accepts besides `default` and `beep`, in the order the
/// `unknown sound` error lists them (upstream's `StatusSoundPlayer.standardNames`).
pub const STANDARD_SOUND_NAMES: [&str; 14] = [
    "Basso",
    "Blow",
    "Bottle",
    "Frog",
    "Funk",
    "Hero",
    "Morse",
    "Ping",
    "Pop",
    "Purr",
    "Sosumi",
    "Submarine",
    "Tink",
    "Glass",
];

/// Whether `name` is a status sound the app knows: `default` or `beep` (the alert sound) or a standard name.
#[must_use]
pub fn is_known_sound(name: &str) -> bool {
    name == "default" || name == "beep" || STANDARD_SOUND_NAMES.contains(&name)
}

/// Whether `hex` is a six-digit hex color, the leading `#` optional (upstream's `WatermarkConfig.isValidColorHex`).
#[must_use]
pub fn is_valid_color_hex(hex: &str) -> bool {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl AppStore {
    /// Applies a control-channel `session.status` write under the pane-precedence rule (upstream's
    /// `applyControlStatus`): while a session is blocked, a write from a DIFFERENT pane is refused unless it is itself
    /// `blocked`, so one agent's `active` or `completed` cannot erase the other pane's block. `idle` is not exempt, as
    /// the bundled hooks emit it unprompted from their own pane. Panes compare after
    /// [`AgentIndicator::normalized_pane`], an untagged write counting as the main pane.
    pub fn apply_control_status(&mut self, indicator: AgentIndicator, id: Uuid) -> StatusWriteResult {
        if let Some(session) = self.session(id)
            && session.agent_indicator.status == AgentStatus::Blocked
            && indicator.status != AgentStatus::Blocked
        {
            let has_split = session.split.exists;
            let owner = session
                .agent_indicator
                .normalized_pane(has_split)
                .unwrap_or(StatusPane::Left);
            let writer = indicator.normalized_pane(has_split).unwrap_or(StatusPane::Left);
            if owner != writer {
                return StatusWriteResult::Refused { owner };
            }
        }
        self.set_agent_indicator(indicator, id);
        StatusWriteResult::Applied
    }

    /// A keystroke in `pane` of session `id` clears its status to idle when that pane owns the status and the
    /// keystroke clears it under `reset` (upstream's `wireStatusClear` closure); returns whether it cleared.
    pub fn clear_status_on_input(
        &mut self,
        id: Uuid,
        pane: StatusPane,
        keystroke: StatusKeystroke,
        reset: StatusReset,
    ) -> bool {
        // a status mirrored from an origin pane this machine has no counterpart for is not this pane's to clear
        let cleared = self.session(id).is_some_and(|session| {
            session
                .remote_presentation()
                .is_none_or(super::RemotePresentationState::allows_keystroke_status_clear)
                && session.agent_indicator.cleared_by(pane, keystroke, reset)
        });
        if cleared {
            self.set_agent_indicator(AgentIndicator::default(), id);
        }
        cleared
    }

    /// Text typed into `pane` of session `id` by `session.type`: the input a blocked agent waited for has arrived, so
    /// the pane-scoped clear runs with the text classified as a submit when it carries a newline and plain typing
    /// otherwise (upstream's `injectAsUserInput`). Empty text clears nothing.
    pub fn note_typed_input(&mut self, id: Uuid, pane: StatusPane, text: &str, reset: StatusReset) {
        if !text.is_empty() {
            self.clear_status_on_input(id, pane, interrupt::classify_text(text), reset);
        }
    }
}
