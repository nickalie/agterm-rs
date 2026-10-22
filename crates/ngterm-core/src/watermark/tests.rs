//! Ported from upstream's `WatermarkConfigTests` and `WatermarkStorageTests`.

use super::*;
use crate::persist::SessionSnapshot;

fn watermark(kind: WatermarkKind) -> BackgroundWatermark {
    BackgroundWatermark {
        kind,
        image_path: None,
        text: None,
        color_hex: None,
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    }
}

fn color(hex: &str) -> BackgroundWatermark {
    BackgroundWatermark {
        color_hex: Some(hex.to_owned()),
        ..watermark(WatermarkKind::Color)
    }
}

#[test]
fn an_image_overlay_emits_every_key() {
    let image = BackgroundWatermark {
        image_path: Some("/tmp/bg.png".to_owned()),
        opacity: Some(0.2),
        fit: Some(WatermarkFit::Cover),
        position: Some(WatermarkPosition::TopLeft),
        repeats: Some(true),
        ..watermark(WatermarkKind::Image)
    };
    let text = overlay_text(Some(&image), Some("/tmp/bg.png"), None, 1.0);
    assert_eq!(
        text,
        "background-opacity = 1\nbackground-image = /tmp/bg.png\nbackground-image-opacity = 0.2\n\
         background-image-fit = cover\nbackground-image-position = top-left\nbackground-image-repeat = true\n"
    );
}

#[test]
fn fit_position_and_repeat_default_and_an_unset_opacity_writes_no_line() {
    let text = overlay_text(Some(&watermark(WatermarkKind::Text)), Some("/x/draft.png"), None, 1.0);
    assert!(text.contains("background-image-fit = contain\n"), "{text}");
    assert!(text.contains("background-image-position = center\n"), "{text}");
    assert!(text.contains("background-image-repeat = false\n"), "{text}");
    assert!(!text.contains("background-image-opacity"), "{text}");
}

#[test]
fn the_font_size_keeps_the_zoom_alongside_or_alone() {
    let image = watermark(WatermarkKind::Image);
    let text = overlay_text(Some(&image), Some("/a.png"), Some(16.0), 1.0);
    assert!(
        text.contains("background-image = /a.png\n") && text.contains("font-size = 16\n"),
        "{text}"
    );
    assert_eq!(overlay_text(None, None, Some(13.5), 1.0), "font-size = 13.5\n");
    assert_eq!(overlay_text(None, None, None, 1.0), "");
}

#[test]
fn a_missing_or_poisoned_path_drops_the_image_but_keeps_the_zoom() {
    let text = watermark(WatermarkKind::Text);
    assert_eq!(overlay_text(Some(&text), None, Some(14.0), 1.0), "font-size = 14\n");
    let poisoned = "/tmp/x.png\nclipboard-read = allow";
    let image = BackgroundWatermark {
        image_path: Some(poisoned.to_owned()),
        ..watermark(WatermarkKind::Image)
    };
    assert_eq!(
        overlay_text(Some(&image), Some(poisoned), Some(14.0), 1.0),
        "font-size = 14\n"
    );
}

#[test]
fn a_color_emits_background_at_the_clamped_window_opacity() {
    assert_eq!(
        overlay_text(Some(&color("#ff0000")), None, None, 1.0),
        "background = #ff0000\nbackground-opacity = 1\n"
    );
    assert_eq!(
        overlay_text(Some(&color("#00ff88")), None, Some(15.0), 0.8),
        "background = #00ff88\nbackground-opacity = 0.8\nfont-size = 15\n"
    );
    let overlay = |opacity: f64| overlay_text(Some(&color("#ff0000")), None, None, opacity);
    assert!(overlay(1.5).contains("background-opacity = 1\n"));
    assert!(overlay(-0.2).contains("background-opacity = 0\n"));
    assert!(overlay(f64::NAN).contains("background-opacity = 1\n"));
    assert!(overlay(f64::INFINITY).contains("background-opacity = 1\n"));
    assert_eq!(
        overlay_text(Some(&color("not-a-color")), None, Some(14.0), 1.0),
        "font-size = 14\n"
    );
}

#[test]
fn numbers_format_without_a_trailing_zero_or_scientific_notation() {
    assert_eq!(formatted(14.0), "14");
    assert_eq!(formatted(0.15), "0.15");
    assert_eq!(formatted(13.5), "13.5");
    assert_eq!(formatted(0.00001), "0.00001");
    assert!(!formatted(0.000_001).contains('e'));
    assert_eq!(formatted(0.2), "0.2");
    assert_eq!(formatted(-0.0), "0");
    assert_eq!(formatted(f64::INFINITY), "inf");
    assert!(!formatted(f64::NAN).is_empty());
    assert_eq!(formatted(1e20), "100000000000000000000");
}

#[test]
fn fit_position_and_opacity_validation_match_ghostty() {
    for fit in ["contain", "cover", "stretch", "none"] {
        assert!(is_valid_fit(fit), "{fit}");
    }
    assert!(!is_valid_fit("fill"));
    assert!(is_valid_position("center") && is_valid_position("bottom-right"));
    assert!(!is_valid_position("middle") && !is_valid_position("center-center"));
    for opacity in [0.0, 0.15, 0.5, 1.0] {
        assert!(is_valid_opacity(opacity), "{opacity}");
    }
    for opacity in [-0.1, 1.0001, 2.0, -100.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert!(!is_valid_opacity(opacity), "{opacity}");
    }
}

#[test]
fn text_path_and_format_validation_match_upstream() {
    assert_eq!(MAX_TEXT_LENGTH, 256);
    assert!(is_valid_text("X") && is_valid_text(&"é".repeat(MAX_TEXT_LENGTH)));
    assert!(!is_valid_text("") && !is_valid_text(&"a".repeat(MAX_TEXT_LENGTH + 1)));
    for path in [
        "/tmp/bg.png",
        "/Users/me/My Pictures/x.png",
        "relative/path.png",
        "/tmp/日本語.png",
        "",
    ] {
        assert!(is_valid_image_path(path), "{path}");
    }
    for path in [
        "x.png\nclipboard-read = allow\ny.png",
        "a.png\r",
        "a\tb.png",
        "a.png\u{0}",
        "\u{1b}[2J",
    ] {
        assert!(!is_valid_image_path(path), "{path:?}");
    }
    assert!(is_supported_image("/a/b.PNG") && is_supported_image("x.jpeg") && is_supported_image("y.jpg"));
    assert!(!is_supported_image("x.gif") && !is_supported_image("png"));
}

#[test]
fn the_osc_overlay_never_writes_the_background_key() {
    assert_eq!(osc_background_overlay_text(None, 0.8), "background-opacity = 0.8\n");
    assert_eq!(
        osc_background_overlay_text(Some(16.0), 1.0),
        "background-opacity = 1\nfont-size = 16\n"
    );
    for (opacity, expected) in [(2.0, "1"), (-1.0, "0"), (f64::NAN, "1")] {
        assert_eq!(
            osc_background_overlay_text(None, opacity),
            format!("background-opacity = {expected}\n")
        );
    }
}

#[test]
fn rendered_text_files_are_named_by_session_and_pane_key() {
    let state = Path::new("/state");
    let id = Uuid::from_u128(0xabc);
    let name = uuid_string(id);
    assert_eq!(directory(state), Path::new("/state/watermarks"));
    assert_eq!(
        rendered_text_path(state, id, None),
        Path::new("/state/watermarks").join(format!("{name}.png"))
    );
    assert_eq!(
        rendered_text_path(state, id, Some("scratch")),
        Path::new("/state/watermarks").join(format!("{name}-scratch.png"))
    );
}

#[test]
fn removal_takes_one_file_or_one_sessions_files_only() -> std::io::Result<()> {
    let state = tempfile::tempdir()?;
    let state = state.path();
    let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
    assert!(!directory(state).exists());
    remove_rendered_text(state, id, None);
    assert_eq!(ensure_directory(state), directory(state));
    assert!(directory(state).is_dir());

    let doomed = [
        rendered_text_path(state, id, None),
        rendered_text_path(state, id, Some(&uuid_string(Uuid::new_v4()))),
        rendered_text_path(state, id, Some("scratch")),
    ];
    let kept = [
        rendered_text_path(state, other, None),
        rendered_text_path(state, other, Some("scratch")),
    ];
    for path in doomed.iter().chain(&kept) {
        std::fs::write(path, b"png")?;
    }
    remove_rendered_text(state, id, Some("scratch"));
    assert!(!doomed[2].exists() && doomed[0].exists());
    remove_rendered_text(state, id, None);
    assert!(!doomed[0].exists() && doomed[1].exists());

    remove_all_rendered_text(state, id);
    assert!(doomed.iter().all(|path| !path.exists()));
    assert!(kept.iter().all(|path| path.exists()));
    remove_all_rendered_text(&state.join("absent"), id);
    Ok(())
}

#[test]
fn a_watermark_survives_a_snapshot_round_trip() -> serde_json::Result<()> {
    let watermark = BackgroundWatermark {
        text: Some("STAGING".to_owned()),
        color_hex: Some("#ffaa00".to_owned()),
        opacity: Some(0.18),
        fit: Some(WatermarkFit::Contain),
        position: Some(WatermarkPosition::Center),
        ..watermark(WatermarkKind::Text)
    };
    let snapshot = SessionSnapshot {
        id: Uuid::new_v4(),
        custom_name: Some("s".to_owned()),
        cwd: "/tmp".to_owned(),
        background_watermark: Some(watermark.clone()),
        ..SessionSnapshot::default()
    };
    let decoded: SessionSnapshot = serde_json::from_str(&serde_json::to_string(&snapshot)?)?;
    assert_eq!(decoded, snapshot);
    assert_eq!(decoded.background_watermark, Some(watermark));
    Ok(())
}

#[test]
fn a_legacy_or_invalid_watermark_decodes_to_none_without_losing_the_session() -> serde_json::Result<()> {
    let id = Uuid::new_v4();
    let legacy: SessionSnapshot = serde_json::from_str(&format!(r#"{{"id":"{id}","cwd":"/tmp"}}"#))?;
    assert_eq!(legacy.background_watermark, None);
    for bad in [
        r#"{"kind":"text","text":"X","fit":"bogus"}"#,
        r#"{"kind":"text","text":"X","position":"middle"}"#,
        r#"{"kind":"hologram","text":"X"}"#,
    ] {
        let raw = format!(r#"{{"id":"{id}","cwd":"/tmp","customName":"keep","backgroundWatermark":{bad}}}"#);
        let decoded: SessionSnapshot = serde_json::from_str(&raw)?;
        assert_eq!(decoded.id, id, "{bad}");
        assert_eq!(decoded.custom_name.as_deref(), Some("keep"), "{bad}");
        assert_eq!(decoded.background_watermark, None, "{bad}");
    }
    Ok(())
}
