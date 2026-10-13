//! The agent-status glyph: a small tinted silhouette for a session's [`AgentIndicator`], drawn by the sidebar rows
//! and the dashboard from one resolver so they cannot drift (upstream's `AppKit` `StatusIconView` in
//! `SidebarRowViews.swift`, its `SwiftUI` twin `StatusGlyph.swift`, and the `statusSymbolName(for:override:)` /
//! `statusColor(for:override:)` resolvers of `GhosttyApp.swift`).
//!
//! [`resolve`] is display-free: it turns an indicator and the settings into a [`GlyphLook`], or `None` for `idle`,
//! which renders nothing. [`StatusGlyph`] draws a look with cairo in place of the SF Symbol `*.fill` silhouettes and
//! pulses its opacity while the look blinks.

use std::cell::{Cell, RefCell};
use std::f64::consts::{PI, TAU};
use std::rc::Rc;

use agterm_core::model::{AgentIndicator, AgentStatus, StatusShape, is_valid_color_hex};
use agterm_core::persist::AppSettings;
use gtk::prelude::*;
use gtk::{cairo, glib};

/// A red-green-blue tint, each channel in `0.0..=1.0`.
pub type Rgb = (f64, f64, f64);

/// The `active` tint when the settings set none: a muted lavender-grey (upstream's `defaultActiveStatusColor`).
const DEFAULT_ACTIVE: Rgb = rgb8(0xDB, 0xD9, 0xE6);
/// The `blocked` tint when the settings set none (macOS `systemOrange`).
const DEFAULT_BLOCKED: Rgb = rgb8(0xFF, 0x95, 0x00);
/// The `completed` tint when the settings set none (macOS `systemGreen`).
const DEFAULT_COMPLETED: Rgb = rgb8(0x34, 0xC7, 0x59);

/// The glyph's slot, square, in logical pixels (upstream's `glyphWidth`).
const SLOT: i32 = 16;
/// The silhouette's extent within the slot (upstream's `glyphPointSize`).
const GLYPH: f64 = 13.0;
/// Half a blink: the fade from full opacity to [`BLINK_LOW`], or back, in microseconds (the frame clock's unit).
const BLINK_HALF_US: i32 = 500_000;
/// The opacity a blink fades down to.
const BLINK_LOW: f64 = 0.2;

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

/// Parses a six-digit hex color, `#` optional and surrounding whitespace ignored (upstream's
/// `NSColor(agtermHex:)`); `None` when malformed.
fn parse_hex(hex: &str) -> Option<Rgb> {
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

/// The blink's opacity `elapsed_us` microseconds into it: a linear fade from 1.0 down to [`BLINK_LOW`] over half a
/// second and back, forever (upstream's autoreversing `CABasicAnimation` on the layer's opacity).
fn blink_opacity(elapsed_us: i64) -> f64 {
    let half = f64::from(BLINK_HALF_US);
    // within one fade-out-and-back period, so it fits an i32
    let phase = f64::from(i32::try_from(elapsed_us.rem_euclid(2 * i64::from(BLINK_HALF_US))).unwrap_or(0));
    let into_fade = if phase < half { phase } else { 2.0 * half - phase };
    1.0 - (1.0 - BLINK_LOW) * into_fade / half
}

/// The glyph's mutable state, shared with its draw function.
#[derive(Default)]
struct GlyphState {
    /// The look drawn; `None` while idle.
    look: RefCell<Option<GlyphLook>>,
    /// The running blink's frame-clock callback.
    blink: RefCell<Option<gtk::TickCallbackId>>,
}

/// The agent-status glyph widget: a 16-pixel slot that collapses entirely while idle, exposed to accessibility under
/// the widget name `agent-status` with the status name as its label (upstream's `StatusIconView`). Cloning it hands
/// out another handle on the same widget.
#[derive(Clone)]
pub struct StatusGlyph {
    area: gtk::DrawingArea,
    state: Rc<GlyphState>,
}

impl StatusGlyph {
    /// A hidden, idle glyph.
    #[must_use]
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .name("agent-status")
            .accessible_role(gtk::AccessibleRole::Img)
            .content_width(SLOT)
            .content_height(SLOT)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        area.update_property(&[gtk::accessible::Property::Label(AgentStatus::Idle.wire_name())]);
        let state = Rc::new(GlyphState::default());
        let drawn = Rc::clone(&state);
        area.set_draw_func(move |_, cr, width, height| {
            if let Some(look) = drawn.look.borrow().as_ref() {
                draw(cr, look, width, height);
            }
        });
        Self { area, state }
    }

    /// The widget to pack into a row.
    #[must_use]
    pub fn widget(&self) -> &gtk::Widget {
        self.area.upcast_ref()
    }

    /// Shows `look`, or hides the slot for `None` (idle): the tooltip and the accessible label follow the status, and
    /// the blink starts or stops with the look's `blink`, but never while animations are off (GTK's reduce-motion
    /// setting). Applying the look already shown leaves a running blink alone.
    pub fn apply(&self, look: Option<&GlyphLook>) {
        let Some(look) = look else {
            self.stop_blink();
            self.state.look.replace(None);
            self.area.set_visible(false);
            self.area.set_tooltip_text(None);
            self.set_accessible_status(AgentStatus::Idle);
            return;
        };
        if self.state.look.borrow().as_ref() != Some(look) {
            self.state.look.replace(Some(look.clone()));
            self.area.set_tooltip_text(Some(&look.tooltip));
            self.set_accessible_status(look.status);
            self.area.queue_draw();
        }
        self.area.set_visible(true);
        if look.blink && self.area.settings().is_gtk_enable_animations() {
            self.start_blink();
        } else {
            self.stop_blink();
        }
    }

    fn set_accessible_status(&self, status: AgentStatus) {
        self.area
            .update_property(&[gtk::accessible::Property::Label(status.wire_name())]);
    }

    /// Starts the opacity pulse unless one runs already; returns whether it started. The frame clock drives it, so it
    /// pauses while the glyph is unmapped (a folded workspace, a scrolled-away row).
    fn start_blink(&self) -> bool {
        if self.state.blink.borrow().is_some() {
            return false;
        }
        let origin = Cell::new(None);
        let id = self.area.add_tick_callback(move |widget, clock| {
            let now = clock.frame_time();
            let start = origin.get().unwrap_or(now);
            origin.set(Some(start));
            widget.set_opacity(blink_opacity(now - start));
            glib::ControlFlow::Continue
        });
        self.state.blink.replace(Some(id));
        true
    }

    /// Stops the opacity pulse, if any, back at full opacity.
    fn stop_blink(&self) {
        if let Some(id) = self.state.blink.take() {
            id.remove();
        }
        self.area.set_opacity(1.0);
    }

    #[cfg(test)]
    fn is_blinking(&self) -> bool {
        self.state.blink.borrow().is_some()
    }
}

impl Default for StatusGlyph {
    fn default() -> Self {
        Self::new()
    }
}

/// Fills `look`'s silhouette, centered in the `width` x `height` slot.
fn draw(cr: &cairo::Context, look: &GlyphLook, width: i32, height: i32) {
    let (red, green, blue) = look.rgb;
    cr.set_source_rgb(red, green, blue);
    cr.translate(f64::from(width) / 2.0, f64::from(height) / 2.0);
    let half = GLYPH / 2.0;
    match look.shape {
        StatusShape::Circle => cr.arc(0.0, 0.0, half - 0.5, 0.0, TAU),
        StatusShape::Square => rounded_rect(cr, half - 1.0, half - 1.0, 2.5),
        StatusShape::Capsule => rounded_rect(cr, half, 3.5, 3.5),
        StatusShape::Diamond => polygon(cr, &[(0.0, -half), (half, 0.0), (0.0, half), (-half, 0.0)]),
        StatusShape::Triangle => polygon(cr, &[(0.0, -5.6), (half, 5.6), (-half, 5.6)]),
        StatusShape::Star => polygon(cr, &star_points(half)),
    }
    // a failed fill leaves the slot blank; nothing to recover
    let _ = cr.fill();
}

/// Traces a rectangle of half-extents `half_width` x `half_height` around the origin with corners of `radius`.
fn rounded_rect(cr: &cairo::Context, half_width: f64, half_height: f64, radius: f64) {
    let (left, right) = (-half_width + radius, half_width - radius);
    let (top, bottom) = (-half_height + radius, half_height - radius);
    cr.new_sub_path();
    cr.arc(right, top, radius, -PI / 2.0, 0.0);
    cr.arc(right, bottom, radius, 0.0, PI / 2.0);
    cr.arc(left, bottom, radius, PI / 2.0, PI);
    cr.arc(left, top, radius, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// Traces a closed polygon through `points`.
fn polygon(cr: &cairo::Context, points: &[(f64, f64)]) {
    let mut points = points.iter();
    if let Some(&(x, y)) = points.next() {
        cr.move_to(x, y);
    }
    for &(x, y) in points {
        cr.line_to(x, y);
    }
    cr.close_path();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtk_test::on_gtk_thread;

    fn indicator(status: AgentStatus) -> AgentIndicator {
        AgentIndicator {
            status,
            ..AgentIndicator::default()
        }
    }

    fn look(status: AgentStatus, settings: &AppSettings) -> GlyphLook {
        resolve(&indicator(status), settings).expect("a non-idle status has a look")
    }

    #[test]
    fn idle_renders_nothing() {
        let settings = AppSettings {
            active_status_color_hex: Some("#112233".into()),
            ..AppSettings::default()
        };
        let idle = AgentIndicator {
            blink: true,
            color: Some("#abcdef".into()),
            shape: Some(StatusShape::Star),
            ..AgentIndicator::default()
        };
        assert_eq!(resolve(&idle, &settings), None);
    }

    #[test]
    fn unset_settings_resolve_to_the_default_tints_and_a_circle() {
        let settings = AppSettings::default();
        let active = look(AgentStatus::Active, &settings);
        assert_eq!(active.rgb, rgb8(0xDB, 0xD9, 0xE6));
        assert_eq!(active.shape, StatusShape::Circle);
        assert!(!active.blink);
        assert_eq!(look(AgentStatus::Blocked, &settings).rgb, rgb8(0xFF, 0x95, 0x00));
        assert_eq!(look(AgentStatus::Completed, &settings).rgb, rgb8(0x34, 0xC7, 0x59));
    }

    #[test]
    fn configured_colors_and_shapes_apply_per_status() {
        let settings = AppSettings {
            active_status_color_hex: Some("#102030".into()),
            blocked_status_color_hex: Some("405060".into()),
            completed_status_color_hex: Some(" #708090 ".into()),
            active_status_shape: Some("square".into()),
            blocked_status_shape: Some("triangle".into()),
            completed_status_shape: Some("star".into()),
            ..AppSettings::default()
        };
        let active = look(AgentStatus::Active, &settings);
        assert_eq!(
            (active.rgb, active.shape),
            (rgb8(0x10, 0x20, 0x30), StatusShape::Square)
        );
        let blocked = look(AgentStatus::Blocked, &settings);
        assert_eq!(
            (blocked.rgb, blocked.shape),
            (rgb8(0x40, 0x50, 0x60), StatusShape::Triangle)
        );
        let completed = look(AgentStatus::Completed, &settings);
        assert_eq!(
            (completed.rgb, completed.shape),
            (rgb8(0x70, 0x80, 0x90), StatusShape::Star)
        );
    }

    #[test]
    fn malformed_configured_values_fall_back_to_the_defaults() {
        let settings = AppSettings {
            blocked_status_color_hex: Some("#12345".into()),
            blocked_status_shape: Some("hexagon".into()),
            ..AppSettings::default()
        };
        let blocked = look(AgentStatus::Blocked, &settings);
        assert_eq!((blocked.rgb, blocked.shape), (DEFAULT_BLOCKED, StatusShape::Circle));
    }

    #[test]
    fn a_valid_per_call_override_wins_over_the_settings() {
        let settings = AppSettings {
            completed_status_color_hex: Some("#102030".into()),
            completed_status_shape: Some("square".into()),
            ..AppSettings::default()
        };
        let indicator = AgentIndicator {
            status: AgentStatus::Completed,
            color: Some("#FFfe01".into()),
            shape: Some(StatusShape::Capsule),
            ..AgentIndicator::default()
        };
        let completed = resolve(&indicator, &settings).expect("completed has a look");
        assert_eq!(
            (completed.rgb, completed.shape),
            (rgb8(0xFF, 0xFE, 0x01), StatusShape::Capsule)
        );
    }

    #[test]
    fn a_malformed_per_call_color_falls_back_to_the_configured_then_the_default() {
        let indicator = AgentIndicator {
            status: AgentStatus::Active,
            color: Some("#zzzzzz".into()),
            ..AgentIndicator::default()
        };
        let configured = AppSettings {
            active_status_color_hex: Some("#102030".into()),
            ..AppSettings::default()
        };
        let resolved = resolve(&indicator, &configured).expect("active has a look");
        assert_eq!(resolved.rgb, rgb8(0x10, 0x20, 0x30));
        let unset = resolve(&indicator, &AppSettings::default()).expect("active has a look");
        assert_eq!(unset.rgb, DEFAULT_ACTIVE);
    }

    #[test]
    fn blink_and_tooltip_follow_the_status() {
        let indicator = AgentIndicator {
            status: AgentStatus::Blocked,
            blink: true,
            ..AgentIndicator::default()
        };
        let blocked = resolve(&indicator, &AppSettings::default()).expect("blocked has a look");
        assert!(blocked.blink);
        assert_eq!(blocked.status, AgentStatus::Blocked);
        assert_eq!(blocked.tooltip, "Agent status: Blocked");
        assert_eq!(
            look(AgentStatus::Active, &AppSettings::default()).tooltip,
            "Agent status: Active"
        );
        assert_eq!(
            look(AgentStatus::Completed, &AppSettings::default()).tooltip,
            "Agent status: Completed"
        );
    }

    #[test]
    fn hex_parsing_accepts_six_digits_with_an_optional_hash() {
        assert_eq!(parse_hex("#000000"), Some((0.0, 0.0, 0.0)));
        assert_eq!(parse_hex("ffffff"), Some((1.0, 1.0, 1.0)));
        for malformed in ["", "#", "#fff", "#1234567", "#12345g", "red"] {
            assert_eq!(parse_hex(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn the_blink_fades_to_a_fifth_and_back_every_second() {
        assert!((blink_opacity(0) - 1.0).abs() < 1e-9);
        assert!((blink_opacity(250_000) - 0.6).abs() < 1e-9);
        assert!((blink_opacity(500_000) - BLINK_LOW).abs() < 1e-9);
        assert!((blink_opacity(750_000) - 0.6).abs() < 1e-9);
        assert!((blink_opacity(1_000_000) - 1.0).abs() < 1e-9);
        assert!((blink_opacity(3_500_000) - BLINK_LOW).abs() < 1e-9);
    }

    #[test]
    fn star_points_alternate_outer_and_inner_and_center_vertically() {
        let points = star_points(6.5);
        let top = points.iter().map(|point| point.1).fold(f64::INFINITY, f64::min);
        let bottom = points.iter().map(|point| point.1).fold(f64::NEG_INFINITY, f64::max);
        assert!((top + bottom).abs() < 1e-9, "top {top}, bottom {bottom}");
        assert!(
            (points[0].0).abs() < 1e-9 && points[0].1 < 0.0,
            "the first point is the top one"
        );
    }

    #[test]
    fn apply_shows_a_look_and_hides_the_slot_for_idle() {
        on_gtk_thread(|| {
            let glyph = StatusGlyph::new();
            let widget = glyph.widget();
            assert_eq!(widget.widget_name(), "agent-status");
            assert_eq!(widget.accessible_role(), gtk::AccessibleRole::Img);
            assert!(!widget.is_visible(), "a new glyph is idle");

            let blocked = look(AgentStatus::Blocked, &AppSettings::default());
            glyph.apply(Some(&blocked));
            assert!(widget.is_visible());
            assert_eq!(widget.tooltip_text().as_deref(), Some("Agent status: Blocked"));
            assert!(!glyph.is_blinking());

            glyph.apply(None);
            assert!(!widget.is_visible());
            assert_eq!(widget.tooltip_text(), None);
        });
    }

    #[test]
    fn a_blinking_look_blinks_until_it_stops_and_reapplying_keeps_the_running_blink() {
        on_gtk_thread(|| {
            let glyph = StatusGlyph::new();
            let mut blocked = look(AgentStatus::Blocked, &AppSettings::default());
            blocked.blink = true;
            glyph.apply(Some(&blocked));
            if !glyph.widget().settings().is_gtk_enable_animations() {
                assert!(!glyph.is_blinking(), "reduce motion skips the blink");
                return;
            }
            assert!(glyph.is_blinking());
            glyph.apply(Some(&blocked));
            assert!(!glyph.start_blink(), "the running blink is kept, not restarted");
            assert!(glyph.is_blinking());

            blocked.blink = false;
            glyph.apply(Some(&blocked));
            assert!(!glyph.is_blinking());
            assert!((glyph.widget().opacity() - 1.0).abs() < 1e-9);

            blocked.blink = true;
            glyph.apply(Some(&blocked));
            glyph.apply(None);
            assert!(!glyph.is_blinking());
            assert!((glyph.widget().opacity() - 1.0).abs() < 1e-9);
        });
    }
}
