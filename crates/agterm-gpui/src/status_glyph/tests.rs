//! The status glyph: the resolver and the blink, ported from the GTK build's `status_glyph.rs` tests; the silhouettes'
//! coverage; and the glyph in GPUI's test window (its slot, its blink running only where animations are on).

use gpui_kit::{Context, Render, TestAppContext, Window, div};

use super::*;

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
    assert_eq!(default_rgb(AgentStatus::Idle), None);
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
    assert_eq!(default_rgb(AgentStatus::Blocked), Some(DEFAULT_BLOCKED));
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
    // the animation's progress through its one-second period
    assert!((blink_at(0.0) - 1.0).abs() < 1e-6);
    assert!((blink_at(0.5) - 0.2).abs() < 1e-6);
    assert!((blink_at(1.0) - 1.0).abs() < 1e-6);
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
fn every_shape_fills_its_own_silhouette_inside_the_slot() {
    let shapes = [
        StatusShape::Circle,
        StatusShape::Square,
        StatusShape::Capsule,
        StatusShape::Diamond,
        StatusShape::Triangle,
        StatusShape::Star,
    ];
    let rendered: Vec<Vec<u8>> = shapes.iter().map(|&shape| rasterize(shape, 32)).collect();
    for (shape, alpha) in shapes.iter().zip(&rendered) {
        // 2x scale: the slot's center is inside every silhouette, its corner outside all
        assert_eq!(alpha[16 * 32 + 16], 255, "{shape:?} is filled at its center");
        assert_eq!(alpha[0], 0, "{shape:?} leaves the corner empty");
    }
    for (i, first) in rendered.iter().enumerate() {
        for second in rendered.iter().skip(i + 1) {
            assert_ne!(first, second, "{:?} looks like another shape", shapes[i]);
        }
    }
    // the capsule is wide and flat: design (8, 3) is outside it, (2, 8) inside
    let capsule = &rendered[2];
    assert_eq!(capsule[6 * 32 + 16], 0);
    assert_eq!(capsule[16 * 32 + 4], 255);
}

/// A view showing one glyph.
struct Shown {
    look: GlyphLook,
    animate: bool,
}

impl Render for Shown {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui_kit::IntoElement {
        div().child(glyph(&self.look, self.animate))
    }
}

#[gpui_kit::test]
fn the_glyph_keeps_its_slot_and_blinks_only_while_animations_run(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    assert!(cx.update(|cx| animations_enabled(cx)), "on while unknown");
    let mut blocked = look(AgentStatus::Blocked, &AppSettings::default());
    blocked.blink = true;
    let (view, cx) = cx.add_window_view(|_, _| Shown {
        look: blocked,
        animate: true,
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("agent-status").expect("the glyph is laid out");
    assert_eq!(bounds.size, gpui_kit::size(px(SLOT), px(SLOT)));
    view.update(cx, |view, cx| {
        view.animate = false;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        cx.set_global(Motion { animations: false });
        assert!(!animations_enabled(cx));
    });
}
