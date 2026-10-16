//! Agent Status: each glyph's color and shape, the blocked sound, which keystroke clears a glyph, auto-follow, and a
//! reset of the glyph settings (upstream's `AgentStatusSettingsView`).

use agterm_core::model::{AgentStatus, StatusReset, StatusShape};
use agterm_core::persist::AppSettings;
use agterm_core::persist::settings::AutoFollowAttention;
use agterm_core::settings_model::{AgentStatusChange, SettingsChange};

use super::form::{Action, Control, Item, Picker, Run, Section, picker, toggle};
use super::notifications::sound_picker;
use super::{GLYPH_STATUSES, Snapshot};
use crate::status_glyph::{self, Rgb};

fn change(change: AgentStatusChange) -> SettingsChange {
    SettingsChange::AgentStatus(change)
}

/// The edit a picked color makes for `status`'s glyph.
#[must_use]
pub fn color_change(status: AgentStatus, hex: String) -> SettingsChange {
    change(AgentStatusChange::Color { status, hex: Some(hex) })
}

/// The page's sections.
#[must_use]
pub fn sections(snapshot: &Snapshot) -> Vec<Section> {
    let settings = &snapshot.settings;
    vec![
        Section::new(
            "Colors and Shapes",
            GLYPH_STATUSES
                .into_iter()
                .map(|status| glyph_row(settings, status))
                .collect(),
        ),
        Section::new(
            "Sound",
            vec![sound_picker(
                "settings-status-blocked-sound",
                "Blocked sound",
                settings.blocked_status_sound_name.as_ref(),
                |name| change(AgentStatusChange::BlockedSound(name)),
            )],
        ),
        Section::new("Typing", vec![status_reset(settings)]),
        Section::new("Auto-follow", auto_follow(settings)),
        Section::untitled(vec![Item::new(
            "settings-status-reset",
            "",
            Control::Button(Action {
                id: "settings-status-reset",
                label: "Reset to defaults",
                run: Run::Change(change(AgentStatusChange::Reset)),
            }),
        )]),
    ]
}

/// One status's row, labelled with the status: its color well and its shape picker, both derived from the one status
/// so a row cannot label one status while driving another's setting.
fn glyph_row(settings: &AppSettings, status: AgentStatus) -> Item {
    let rgb = glyph_rgb(settings, status);
    let shape = settings.effective_status_shape(status).unwrap_or(StatusShape::Circle);
    let shapes = StatusShape::ALL
        .into_iter()
        .map(|shape| (title_case(shape.wire_name()), shape))
        .collect();
    let mut picker = Picker::new(shapes, &shape, move |shape| {
        change(AgentStatusChange::Shape { status, shape })
    });
    picker.shapes = Some(rgb);
    Item::new(
        format!("settings-status-shape-{}", status.wire_name()),
        title_case(status.wire_name()),
        Control::Glyph {
            status,
            rgb,
            shape: picker,
        },
    )
}

/// A name with its first letter upper-cased: `Active`, `Diamond`.
#[must_use]
pub fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The glyph's tint: the configured color, else the status's default.
#[must_use]
pub fn glyph_rgb(settings: &AppSettings, status: AgentStatus) -> Rgb {
    settings
        .status_color_hex(status)
        .and_then(status_glyph::parse_hex)
        .or_else(|| status_glyph::default_rgb(status))
        .unwrap_or((0.0, 0.0, 0.0))
}

fn status_reset(settings: &AppSettings) -> Item {
    picker(
        "settings-status-clear",
        "Status reset",
        &[
            ("On first key", StatusReset::FirstKey),
            ("On Enter", StatusReset::Enter),
            ("Disabled", StatusReset::Never),
        ],
        &settings.effective_status_reset(),
        |reset| change(AgentStatusChange::StatusReset(reset)),
    )
}

fn auto_follow(settings: &AppSettings) -> Vec<Item> {
    let follow = settings.effective_auto_follow_attention();
    vec![
        picker(
            "settings-auto-follow",
            "Auto-follow blocked sessions",
            &[
                ("Disabled", AutoFollowAttention::Off),
                ("5 sec idle", AutoFollowAttention::S5),
                ("10 sec idle", AutoFollowAttention::S10),
                ("30 sec idle", AutoFollowAttention::S30),
                ("60 sec idle", AutoFollowAttention::S60),
                ("5 min idle", AutoFollowAttention::M5),
            ],
            &follow,
            |follow| change(AgentStatusChange::AutoFollow(follow)),
        ),
        toggle(
            "settings-auto-follow-stay-active",
            "Auto-follow away from a running session",
            !settings.auto_follow_stay_on_active.unwrap_or(false),
            |away| change(AgentStatusChange::AutoFollowAwayFromRunning(away)),
        )
        .enabled(follow != AutoFollowAttention::Off),
        Item::hint("settings-auto-follow-hint", "Only applies while auto-follow is on."),
    ]
}
