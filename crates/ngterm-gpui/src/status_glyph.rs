//! The agent-status glyph: a small tinted silhouette for a session's [`AgentIndicator`], drawn by the sidebar rows from
//! one resolver, which the dashboard's status chip is to share so the two cannot drift (ported from the GTK build's
//! `status_glyph.rs`; upstream's `AppKit` `StatusIconView` in `SidebarRowViews.swift`, its `SwiftUI` twin
//! `StatusGlyph.swift`, and the `statusSymbolName(for:override:)` / `statusColor(for:override:)` resolvers of
//! `GhosttyApp.swift`).
//!
//! [`resolve`] is display-free: it turns an indicator and the settings into a [`GlyphLook`], or `None` for `idle`,
//! which renders nothing. [`glyph`] draws a look in place of the SF Symbol `*.fill` silhouettes (traced like the row
//! icons, [`crate::row_icon`]) and pulses its opacity while the look blinks, unless the desktop turned animations off
//! ([`Motion`], GTK's `gtk-enable-animations`, upstream's reduce-motion check).

#[cfg(test)]
mod tests;

use std::f64::consts::{PI, TAU};
use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, App, Bounds, Global, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Rgba, SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};
use ngterm_core::model::{AgentIndicator, AgentStatus, StatusShape, is_valid_color_hex};
use ngterm_core::persist::AppSettings;

use crate::row_icon::ink::{Ink, Trace};
use crate::row_icon::{Mask, SLOT, paint_mask};

/// A red-green-blue tint, each channel in `0.0..=1.0`.
pub type Rgb = (f64, f64, f64);

/// The `active` tint when the settings set none: a muted lavender-grey (upstream's `defaultActiveStatusColor`).
const DEFAULT_ACTIVE: Rgb = rgb8(0xDB, 0xD9, 0xE6);
/// The `blocked` tint when the settings set none (macOS `systemOrange`).
const DEFAULT_BLOCKED: Rgb = rgb8(0xFF, 0x95, 0x00);
/// The `completed` tint when the settings set none (macOS `systemGreen`).
const DEFAULT_COMPLETED: Rgb = rgb8(0x34, 0xC7, 0x59);

/// The design units a silhouette is traced in: the 16-point slot.
const BOX: f64 = 16.0;
/// The silhouette's extent within the slot (upstream's `glyphPointSize`).
const GLYPH: f64 = 13.0;
/// Half a blink: the fade from full opacity to [`BLINK_LOW`], or back, in microseconds.
const BLINK_HALF_US: i32 = 500_000;
/// The opacity a blink fades down to.
const BLINK_LOW: f64 = 0.2;
/// The blink's redraw rate: a fade needs no more.
const BLINK_FPS: f32 = 30.0;

/// How a non-idle status glyph looks, resolved from the session's indicator and the settings.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphLook {
    /// The status shown; never [`AgentStatus::Idle`].
    pub status: AgentStatus,
    /// The silhouette drawn.
    pub shape: StatusShape,
    /// The tint the silhouette is filled with.
    pub rgb: Rgb,
    /// Whether the glyph pulses for attention (`session.status --blink`).
    pub blink: bool,
    /// The hover text, `Agent status: <Status>`.
    pub tooltip: String,
}

/// Resolves how `indicator`'s glyph looks; `None` for `idle`, which renders no glyph.
///
/// The shape is the per-call override (`session.status --shape`), else the status's configured settings shape, else a
/// circle (upstream's `AgentStatus.symbolName(override:configured:)`). The tint is a valid per-call `#rrggbb` override
/// (`--color`), else the status's configured settings color, else the status's default; a malformed hex counts as
/// unset (upstream's `statusColor(for:override:)` over `setAgentStatusColors`).
#[must_use]
pub fn resolve(indicator: &AgentIndicator, settings: &AppSettings) -> Option<GlyphLook> {
    let (configured_hex, configured_shape, default_rgb) = match indicator.status {
        AgentStatus::Idle => return None,
        AgentStatus::Active => (
            &settings.active_status_color_hex,
            &settings.active_status_shape,
            DEFAULT_ACTIVE,
        ),
        AgentStatus::Blocked => (
            &settings.blocked_status_color_hex,
            &settings.blocked_status_shape,
            DEFAULT_BLOCKED,
        ),
        AgentStatus::Completed => (
            &settings.completed_status_color_hex,
            &settings.completed_status_shape,
            DEFAULT_COMPLETED,
        ),
    };
    let rgb = indicator
        .color
        .as_deref()
        .and_then(parse_hex)
        .or_else(|| configured_hex.as_deref().and_then(parse_hex))
        .unwrap_or(default_rgb);
    let shape = indicator
        .shape
        .or_else(|| configured_shape.as_deref().and_then(StatusShape::from_wire))
        .unwrap_or(StatusShape::Circle);
    Some(GlyphLook {
        status: indicator.status,
        shape,
        rgb,
        blink: indicator.blink,
        tooltip: tooltip(indicator.status),
    })
}

/// The hover text for a visible glyph (upstream's `AgentStatus.tooltipText`): `Agent status: Blocked`.
fn tooltip(status: AgentStatus) -> String {
    let name = status.wire_name();
    let mut chars = name.chars();
    let capitalized: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    format!("Agent status: {capitalized}")
}

/// The tint a status's glyph has when the settings set none; `None` for `idle`, which draws no glyph.
#[must_use]
pub fn default_rgb(status: AgentStatus) -> Option<Rgb> {
    match status {
        AgentStatus::Idle => None,
        AgentStatus::Active => Some(DEFAULT_ACTIVE),
        AgentStatus::Blocked => Some(DEFAULT_BLOCKED),
        AgentStatus::Completed => Some(DEFAULT_COMPLETED),
    }
}

/// Parses a six-digit hex color, `#` optional and surrounding whitespace ignored (upstream's
/// `NSColor(agtermHex:)`); `None` when malformed.
#[must_use]
pub fn parse_hex(hex: &str) -> Option<Rgb> {
    let hex = hex.trim();
    if !is_valid_color_hex(hex) {
        return None;
    }
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()?;
    let [_, red, green, blue] = value.to_be_bytes();
    Some(rgb8(red, green, blue))
}

/// An 8-bit-per-channel color as an [`Rgb`].
const fn rgb8(red: u8, green: u8, blue: u8) -> Rgb {
    (red as f64 / 255.0, green as f64 / 255.0, blue as f64 / 255.0)
}

/// `rgb` as GPUI's color, opaque.
#[must_use]
pub fn hsla(rgb: Rgb) -> Hsla {
    #[expect(clippy::cast_possible_truncation, reason = "channels within 0...1")]
    let channel = |value: f64| value.clamp(0.0, 1.0) as f32;
    Rgba {
        r: channel(rgb.0),
        g: channel(rgb.1),
        b: channel(rgb.2),
        a: 1.0,
    }
    .into()
}

/// The blink's opacity `elapsed_us` microseconds into it: a linear fade from 1.0 down to [`BLINK_LOW`] over half a
/// second and back, forever (upstream's autoreversing `CABasicAnimation` on the layer's opacity).
fn blink_opacity(elapsed_us: i64) -> f64 {
    let half = f64::from(BLINK_HALF_US);
    // within one fade-out-and-back period, so it fits an i32
    let phase = f64::from(i32::try_from(elapsed_us.rem_euclid(2 * i64::from(BLINK_HALF_US))).unwrap_or(0));
    let into_fade = if phase < half { phase } else { 2.0 * half - phase };
    1.0 - (1.0 - BLINK_LOW) * into_fade / half
}

/// The blink's opacity at `delta` (0...1) through its one-second period.
fn blink_at(delta: f32) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a fraction of one second in microseconds"
    )]
    let elapsed = (f64::from(delta.clamp(0.0, 1.0)) * f64::from(2 * BLINK_HALF_US)).round() as i64;
    #[expect(clippy::cast_possible_truncation, reason = "an opacity within 0...1")]
    let opacity = blink_opacity(elapsed) as f32;
    opacity
}

/// Whether the desktop lets things animate: GTK's `gtk-enable-animations`, read from the settings portal's
/// `org.gnome.desktop.interface` `enable-animations` and kept current by its `SettingChanged` ([`watch_motion`]). On
/// while unknown (no portal, a desktop that does not carry the key), as GTK's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Motion {
    pub animations: bool,
}

impl Global for Motion {}

impl Default for Motion {
    fn default() -> Self {
        Self { animations: true }
    }
}

/// Whether blinks run now.
#[must_use]
pub fn animations_enabled(cx: &App) -> bool {
    cx.try_global::<Motion>().is_none_or(|motion| motion.animations)
}

/// The portal namespace and key of the desktop's animation switch.
const MOTION_NAMESPACE: &str = "org.gnome.desktop.interface";
const MOTION_KEY: &str = "enable-animations";

/// Starts following the desktop's animation switch, once per app; every window redraws when it flips.
pub fn watch_motion(cx: &mut App) {
    if cx.has_global::<Motion>() {
        return;
    }
    cx.set_global(Motion::default());
    cx.spawn(async move |cx| {
        let Ok(settings) = ashpd::desktop::settings::Settings::new().await else {
            tracing::debug!("no settings portal: status glyphs blink");
            return;
        };
        if let Ok(animations) = settings.read::<bool>(MOTION_NAMESPACE, MOTION_KEY).await {
            set_motion(cx, animations);
        }
        let Ok(mut changes) = settings
            .receive_setting_changed_with_args::<bool>(MOTION_NAMESPACE, MOTION_KEY)
            .await
        else {
            return;
        };
        while let Some(change) = futures_util::StreamExt::next(&mut changes).await {
            if let Ok(animations) = change {
                set_motion(cx, animations);
            }
        }
    })
    .detach();
}

fn set_motion(cx: &mut gpui_kit::AsyncApp, animations: bool) {
    cx.update(|cx| {
        if animations_enabled(cx) != animations {
            tracing::debug!(animations, "the desktop's animation switch changed");
            cx.set_global(Motion { animations });
            cx.refresh_windows();
        }
    });
}

/// `look`'s glyph in its 16-point slot, exposed under the element id `agent-status` with the status's hover text; it
/// pulses while the look blinks and `animate` allows (the blink keeps running across redraws: the animation is keyed
/// by the element, so reapplying the look shown leaves it alone).
#[must_use]
pub fn glyph(look: &GlyphLook, animate: bool) -> impl IntoElement {
    let shape = look.shape;
    let color = hsla(look.rgb);
    let tooltip = SharedString::from(look.tooltip.clone());
    let blinking = look.blink && animate;
    let silhouette = div()
        .debug_selector(move || {
            if blinking {
                "agent-status-blinking".to_owned()
            } else {
                "agent-status-still".to_owned()
            }
        })
        .size(px(SLOT))
        .child(
            canvas(
                |_, _, _| {},
                move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
                    paint_mask(window, bounds, color, Mask::Status(shape));
                },
            )
            .size(px(SLOT)),
        );
    let slot = div()
        .id("agent-status")
        .accessibility_id("agent-status")
        .role(gpui_kit::Role::Image)
        .aria_label(tooltip.clone())
        .debug_selector(|| "agent-status".to_owned())
        .flex_none()
        .size(px(SLOT))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
        .child(silhouette);
    if blinking {
        let period = Duration::from_micros(u64::from(2 * BLINK_HALF_US.unsigned_abs()));
        slot.with_animation(
            "agent-status-blink",
            Animation::new(period).repeat().with_max_fps(BLINK_FPS),
            |slot, delta| slot.opacity(blink_at(delta)),
        )
        .into_any_element()
    } else {
        slot.into_any_element()
    }
}

/// `shape`'s silhouette centered on a `side` x `side` canvas, row-major coverage.
#[must_use]
pub fn rasterize(shape: StatusShape, side: u32) -> Vec<u8> {
    let mut ink = Ink::new(side, BOX);
    let center = BOX / 2.0;
    let half = GLYPH / 2.0;
    let mut trace = Trace::new();
    let at = |(x, y): (f64, f64)| (center + x, center + y);
    match shape {
        StatusShape::Circle => trace.arc((center, center, half - 0.5), 0.0, TAU),
        StatusShape::Square => trace.rounded_rect(
            (
                center - half + 1.0,
                center - half + 1.0,
                2.0 * (half - 1.0),
                2.0 * (half - 1.0),
            ),
            2.5,
        ),
        StatusShape::Capsule => trace.rounded_rect((center - half, center - 3.5, 2.0 * half, 7.0), 3.5),
        StatusShape::Diamond => {
            trace.polygon(&[at((0.0, -half)), at((half, 0.0)), at((0.0, half)), at((-half, 0.0))]);
        }
        StatusShape::Triangle => trace.polygon(&[at((0.0, -5.6)), at((half, 5.6)), at((-half, 5.6))]),
        StatusShape::Star => trace.polygon(&star_points(half).map(at)),
    }
    ink.fill(trace);
    ink.coverage()
}

/// A five-point star of outer radius `outer`, point up, nudged down so its points' extent centers on the origin.
fn star_points(outer: f64) -> [(f64, f64); 10] {
    let inner = outer * 0.45;
    // the top point reaches -outer, the two lower points only outer * cos 36°
    let nudge = outer * (1.0 - (PI / 5.0).cos()) / 2.0;
    let mut points = [(0.0, 0.0); 10];
    for (index, point) in (0_u8..).zip(points.iter_mut()) {
        let angle = -PI / 2.0 + f64::from(index) * PI / 5.0;
        let radius = if index % 2 == 0 { outer } else { inner };
        *point = (radius * angle.cos(), radius * angle.sin() + nudge);
    }
    points
}
