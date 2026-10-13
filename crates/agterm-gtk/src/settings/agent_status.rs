//! Agent Status: each glyph's color and shape, the blocked sound, which keystroke clears a glyph, auto-follow, and a
//! reset of the glyph settings (upstream's `AgentStatusSettingsView`).

use adw::prelude::*;
use agterm_core::model::{AgentStatus, StatusReset, StatusShape};
use agterm_core::persist::settings::AutoFollowAttention;
use agterm_core::settings_model::{AgentStatusChange, SettingsChange};

use super::notifications::{preview_sound, sound_options};
use super::{Rows, Snapshot, refreshing, rows, send};
use crate::status_glyph::{self, Rgb};

fn change(change: AgentStatusChange) -> SettingsChange {
    SettingsChange::AgentStatus(change)
}

pub fn page(rows: &mut Rows) -> adw::PreferencesPage {
    let page = rows::page("Agent Status", "agent-status", "emblem-system-symbolic");
    let glyphs = rows::group("Colors and Shapes");
    for status in [AgentStatus::Active, AgentStatus::Blocked, AgentStatus::Completed] {
        glyphs.add(&glyph_row(rows, status));
    }
    page.add(&glyphs);
    page.add(&sound(rows));
    page.add(&typing(rows));
    page.add(&auto_follow(rows));
    page.add(&reset(rows));
    page
}

/// One status's row: its shape picker titled with the status, its color button beside, both derived from the one
/// status so a row cannot label one status while driving another's setting.
fn glyph_row(rows: &mut Rows, status: AgentStatus) -> adw::ComboRow {
    let shapes: Vec<(String, StatusShape)> = StatusShape::ALL
        .into_iter()
        .map(|shape| (title_case(shape.wire_name()), shape))
        .collect();
    let row = rows::choice(
        rows,
        &format!("settings-status-shape-{}", status.wire_name()),
        &title_case(status.wire_name()),
        move |_| shapes.clone(),
        move |snapshot| {
            snapshot
                .settings
                .effective_status_shape(status)
                .unwrap_or(StatusShape::Circle)
        },
        move |shape| change(AgentStatusChange::Shape { status, shape }),
    );
    let dialog = gtk::ColorDialog::builder().with_alpha(false).build();
    let color = gtk::ColorDialogButton::new(Some(dialog));
    color.set_valign(gtk::Align::Center);
    color.set_widget_name(&format!("settings-status-{}", status.wire_name()));
    let poster = rows.poster().clone();
    color.connect_rgba_notify(move |button| {
        if !refreshing() {
            let hex = hex_of(&button.rgba());
            send(&poster, change(AgentStatusChange::Color { status, hex: Some(hex) }));
        }
    });
    let shown = color.clone();
    rows.refresh(move |snapshot| shown.set_rgba(&rgba(glyph_rgb(snapshot, status))));
    row.add_suffix(&color);
    row
}

/// A name with its first letter upper-cased: `Active`, `Diamond`.
fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The glyph's tint: the configured color, else the status's default.
fn glyph_rgb(snapshot: &Snapshot, status: AgentStatus) -> Rgb {
    snapshot
        .settings
        .status_color_hex(status)
        .and_then(status_glyph::parse_hex)
        .or_else(|| status_glyph::default_rgb(status))
        .unwrap_or((0.0, 0.0, 0.0))
}

#[expect(clippy::cast_possible_truncation, reason = "channels are within 0.0...1.0")]
fn rgba((red, green, blue): Rgb) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(red as f32, green as f32, blue as f32, 1.0)
}

/// A picked color as the settings store it: `#rrggbb`, lower-case.
fn hex_of(color: &gtk::gdk::RGBA) -> String {
    let channel = |value: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0...255 first"
        )]
        let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        byte
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.red()),
        channel(color.green()),
        channel(color.blue())
    )
}

fn sound(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Sound");
    group.add(&rows::choice(
        rows,
        "settings-status-blocked-sound",
        "Blocked sound",
        |_| sound_options(),
        |snapshot| snapshot.settings.blocked_status_sound_name.clone(),
        |name| {
            preview_sound(name.as_deref());
            change(AgentStatusChange::BlockedSound(name))
        },
    ));
    group
}

fn typing(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Typing");
    group.add(&rows::picker(
        rows,
        "settings-status-clear",
        "Status reset",
        &[
            ("On first key", StatusReset::FirstKey),
            ("On Enter", StatusReset::Enter),
            ("Disabled", StatusReset::Never),
        ],
        |snapshot| snapshot.settings.effective_status_reset(),
        |reset| change(AgentStatusChange::StatusReset(reset)),
    ));
    group
}

fn auto_follow(rows: &mut Rows) -> adw::PreferencesGroup {
    let group = rows::group("Auto-follow");
    group.add(&rows::picker(
        rows,
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
        |snapshot| snapshot.settings.effective_auto_follow_attention(),
        |follow| change(AgentStatusChange::AutoFollow(follow)),
    ));
    let away = rows::switch(
        rows,
        "settings-auto-follow-stay-active",
        "Auto-follow away from a running session",
        |snapshot| !snapshot.settings.auto_follow_stay_on_active.unwrap_or(false),
        |away| change(AgentStatusChange::AutoFollowAwayFromRunning(away)),
    );
    away.set_subtitle("Only applies while auto-follow is on.");
    rows::enable_when(rows, &away, |snapshot| {
        snapshot.settings.effective_auto_follow_attention() != AutoFollowAttention::Off
    });
    group.add(&away);
    group
}

fn reset(rows: &Rows) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let button = gtk::Button::builder()
        .label("Reset to defaults")
        .name("settings-status-reset")
        .halign(gtk::Align::Start)
        .build();
    let poster = rows.poster().clone();
    button.connect_clicked(move |_| send(&poster, change(AgentStatusChange::Reset)));
    group.add(&button);
    group
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_stored_as_lower_case_hex() {
        assert_eq!(hex_of(&gtk::gdk::RGBA::new(1.0, 0.5, 0.0, 1.0)), "#ff8000");
        assert_eq!(title_case(AgentStatus::Blocked.wire_name()), "Blocked");
        assert_eq!(title_case("diamond"), "Diamond");
    }
}
