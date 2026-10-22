//! The HUD's host-free half (upstream's `Hud.swift` and `HudMarkdown.swift`): the spec a `session.hud.open`/`.update`
//! posts, its spinner and position vocabularies, the panel's sizing math and the exact bytes the bundled painter
//! (`hud.sh`) reads from its body file.

use std::ops::RangeInclusive;

pub mod layout;
pub mod markdown;

pub use layout::{
    FILE_ENV_KEY, Grid, HudPaneFrame, HudPaneFrames, HudPanelSize, MAX_COLUMNS, MAX_SIZE_PERCENT, MIN_SIZE_PERCENT,
    MarkdownBody, PaneMetrics, box_for, clamp_size_percent, clamp_size_percent_for, foreground_sgr, max_width_percent,
    paint_grid, panel_grid, panel_size, rendered_body, text_length,
};

/// The message a control client posts over a session while it prepares something (upstream's `HudSpec`). `position`
/// carries its default when the caller omitted it, so a spec always holds the effective value the read-back reports;
/// a `None` spinner is a static panel.
#[derive(Debug, Clone, PartialEq)]
pub struct HudSpec {
    pub message: String,
    pub detail: Option<String>,
    /// The spinner's style, `None` for a static panel.
    pub spinner: Option<HudSpinner>,
    /// `#rrggbb` background of the panel's surface; `None` keeps the terminal background. Read once at creation.
    pub background_color: Option<String>,
    /// `#rrggbb` of the panel's text; `None` keeps the terminal foreground. Rides the body file, so an update
    /// repaints it.
    pub text_color: Option<String>,
    /// The caller's override of the panel's share of the pane WIDTH; `None` measures it from the message.
    pub size_percent: Option<i64>,
    pub position: HudPosition,
    /// Seconds after which the panel takes itself down; `None` or 0 stays.
    pub hide_after: Option<f64>,
    /// Renders `message` as markdown instead of centered plain text.
    pub markdown: bool,
    /// The panel's point size, `None` to inherit the session's. Read once at creation.
    pub font_size: Option<f64>,
    /// Drops the edge margin, so the panel sits flush against the edge or corner `position` names.
    pub sticky: bool,
    /// `false` drops the border, the rounding and the blank row above and below the text.
    pub frame: bool,
}

impl HudSpec {
    /// The cap on `detail` and a plain `message`, in [`text_length`]'s unit.
    pub const MAX_TEXT_LENGTH: usize = 256;
    /// The cap on a markdown `message`.
    pub const MAX_MARKDOWN_LENGTH: usize = 4096;
    /// The longest auto-hide on offer, in seconds.
    pub const MAX_HIDE_AFTER: f64 = 86_400.0;
    /// The point sizes `--font-size` accepts.
    pub const FONT_SIZE_RANGE: RangeInclusive<f64> = 6.0..=72.0;

    /// A spec of `message` with every other field at upstream's default.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: None,
            spinner: None,
            background_color: None,
            text_color: None,
            size_percent: None,
            position: HudPosition::DEFAULT,
            hide_after: None,
            markdown: false,
            font_size: None,
            sticky: false,
            frame: true,
        }
    }

    #[must_use]
    pub fn is_valid_font_size(points: f64) -> bool {
        Self::FONT_SIZE_RANGE.contains(&points)
    }

    /// Whether `seconds` can be scheduled: finite and within `0...MAX_HIDE_AFTER`. Rejected rather than clamped.
    #[must_use]
    pub fn is_valid_hide_after(seconds: f64) -> bool {
        seconds.is_finite() && (0.0..=Self::MAX_HIDE_AFTER).contains(&seconds)
    }

    /// The panel's own auto-hide, 0 when it stays.
    #[must_use]
    pub fn effective_hide_after(&self) -> f64 {
        self.hide_after.unwrap_or(0.0)
    }

    /// This spec with the live panel's background and font size, which the surface reads only at creation.
    #[must_use]
    pub fn holding_creation_fields(&self, live: &Self) -> Self {
        Self {
            background_color: live.background_color.clone(),
            font_size: live.font_size,
            ..self.clone()
        }
    }

    /// This spec at another width override.
    #[must_use]
    pub fn with_size_percent(&self, percent: Option<i64>) -> Self {
        Self {
            size_percent: percent,
            ..self.clone()
        }
    }
}

/// The animated glyph a spinning panel shows beside its message (upstream's `HudSpinner`). Every frame is one
/// single-column Unicode scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HudSpinner {
    /// ASCII, so it renders in any font; the default.
    Bar,
    Braille,
    Circle,
    Blocks,
    /// A dot that blinks off rather than animating.
    Dot,
}

impl HudSpinner {
    /// Every style, in upstream's order.
    pub const ALL: [Self; 5] = [Self::Bar, Self::Braille, Self::Circle, Self::Blocks, Self::Dot];
    /// The style a caller who asks for a spinner without naming one gets.
    pub const DEFAULT_STYLE: Self = Self::Bar;
    /// The read-back's spelling for a panel with no spinner, accepted back as input.
    pub const NONE_NAME: &'static str = "none";
    /// The tick of a panel with no spinner: how often it re-reads the body file.
    pub const STATIC_INTERVAL: &'static str = "0.5";

    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Braille => "braille",
            Self::Circle => "circle",
            Self::Blocks => "blocks",
            Self::Dot => "dot",
        }
    }

    /// The style named `raw`; `None` for anything else, `none` included.
    #[must_use]
    pub fn from_wire(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.wire_name() == raw)
    }

    /// The style names pipe-joined.
    #[must_use]
    pub fn valid_names_list() -> String {
        Self::names(false).join("|")
    }

    /// Everything a caller may pass, `none` included, pipe-joined: the control server's rejection message.
    #[must_use]
    pub fn accepted_names_list() -> String {
        Self::names(true).join("|")
    }

    /// The accepted set comma-joined: `ngtermctl --spinner-style` help and its local rejection.
    #[must_use]
    pub fn accepted_names_phrase() -> String {
        Self::names(true).join(", ")
    }

    fn names(with_none: bool) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Self::ALL.into_iter().map(Self::wire_name).collect();
        if with_none {
            names.push(Self::NONE_NAME);
        }
        names
    }

    /// The animation frames, one single-column scalar each. The dot's blank half is a NO-BREAK SPACE, which the
    /// painter's word splitting keeps.
    #[must_use]
    pub fn frames(self) -> &'static [&'static str] {
        match self {
            Self::Bar => &["|", "/", "-", "\\"],
            Self::Braille => &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            Self::Circle => &["◐", "◓", "◑", "◒"],
            Self::Blocks => &["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"],
            Self::Dot => &["●", "\u{00A0}"],
        }
    }

    /// Seconds between frames, as the literal text the painter hands `sleep`.
    #[must_use]
    pub fn interval(self) -> &'static str {
        match self {
            Self::Bar => "0.1",
            Self::Braille | Self::Blocks => "0.08",
            Self::Circle => "0.12",
            Self::Dot => "0.45",
        }
    }
}

/// One axis' term of an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Leading,
    Middle,
    Trailing,
}

/// Where the panel sits in the pane: the nine anchors of a 3x3 grid, spelled as the background watermark's
/// (upstream's `HudPosition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HudPosition {
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

impl HudPosition {
    /// Every anchor, in upstream's order.
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
    /// The bare spellings kept accepted, normalized to the middle column.
    pub const ALIASES: [(&'static str, Self); 2] = [("top", Self::TopCenter), ("bottom", Self::BottomCenter)];
    /// The placement a caller who omits `--position` gets.
    pub const DEFAULT: Self = Self::Center;
    /// Percent of the pane held clear at an edge the panel is anchored to, unless sticky.
    pub const EDGE_MARGIN_PERCENT: i64 = 10;

    /// The canonical spelling.
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

    /// A caller's spelling resolved, aliases included: the one entry point every path takes.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|position| position.wire_name() == raw)
            .or_else(|| {
                Self::ALIASES
                    .into_iter()
                    .find(|(name, _)| *name == raw)
                    .map(|(_, position)| position)
            })
    }

    /// Which row the anchor names.
    #[must_use]
    pub fn vertical_band(self) -> Band {
        match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => Band::Leading,
            Self::CenterLeft | Self::Center | Self::CenterRight => Band::Middle,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => Band::Trailing,
        }
    }

    /// Which column the anchor names.
    #[must_use]
    pub fn horizontal_band(self) -> Band {
        match self {
            Self::TopLeft | Self::CenterLeft | Self::BottomLeft => Band::Leading,
            Self::TopCenter | Self::Center | Self::BottomCenter => Band::Middle,
            Self::TopRight | Self::CenterRight | Self::BottomRight => Band::Trailing,
        }
    }

    /// The canonical names pipe-joined.
    #[must_use]
    pub fn valid_names_list() -> String {
        Self::names(false).join("|")
    }

    /// The canonical names comma-joined.
    #[must_use]
    pub fn valid_names_phrase() -> String {
        Self::names(false).join(", ")
    }

    /// Everything a caller may pass, aliases included, pipe-joined: the control server's rejection message.
    #[must_use]
    pub fn accepted_names_list() -> String {
        Self::names(true).join("|")
    }

    /// The accepted set comma-joined: `ngtermctl --position` help and its local rejection.
    #[must_use]
    pub fn accepted_names_phrase() -> String {
        Self::names(true).join(", ")
    }

    fn names(with_aliases: bool) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Self::ALL.into_iter().map(Self::wire_name).collect();
        if with_aliases {
            names.extend(Self::ALIASES.into_iter().map(|(name, _)| name));
        }
        names
    }
}

#[cfg(test)]
mod tests;
