//! The embedded fonts (plan > Task 15): which faces register, and that Ghostty's default face measures through GPUI's
//! Linux text system (`CosmicTextSystem`, with no system font, so nothing installed can stand in) as Ghostty measures
//! it, its line gap and underline included (`TextSystem::font_metrics`, GPUI patch 0002).

use std::sync::Arc;

use gpui_kit::{TestAppContext, TestDispatcher, TextRun, px};

use super::super::metrics::{self, CellMetrics};
use super::{DEFAULT_FAMILY, Fonts, SYMBOLS_FAMILY, embedded_faces, register_embedded};

/// A test app whose text system knows only the embedded fonts.
fn embedded_only() -> TestAppContext {
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(DEFAULT_FAMILY));
    let cx = TestAppContext::build_with_text_system(TestDispatcher::new(0), None, text_system);
    register_embedded(cx.text_system());
    cx
}

#[test]
fn every_embedded_face_registers_unless_the_system_has_jetbrains_mono() {
    assert_eq!(
        embedded_faces(&[]).len(),
        5,
        "JetBrains Mono's four faces and the symbols font"
    );
    let system = ["JetBrains Mono".to_owned(), "Hack".to_owned()];
    assert_eq!(
        embedded_faces(&system).len(),
        1,
        "a system JetBrains Mono stands, the symbols font still comes"
    );
}

#[test]
fn the_default_face_is_the_embedded_jetbrains_mono_with_the_symbols_fallback() {
    let cx = embedded_only();
    let text = cx.text_system().clone();
    let installed = text.all_font_names();
    for family in [DEFAULT_FAMILY, SYMBOLS_FAMILY] {
        assert!(installed.iter().any(|name| name == family), "{family} in {installed:?}");
    }
    let fonts = Fonts::resolve(&agterm_core::ghostty_config::Font::default(), &|family| {
        installed.iter().any(|name| name == family)
    });
    assert_eq!(fonts.regular.family.as_ref(), DEFAULT_FAMILY);
    let fallbacks = fonts.regular.fallbacks.as_ref().map(|f| f.fallback_list().to_vec());
    assert_eq!(fallbacks, Some(vec![SYMBOLS_FAMILY.to_owned()]));
    // a configured family that is missing falls back to the default face too
    let missing = agterm_core::ghostty_config::Font {
        family: vec!["Missing Mono".into()],
        ..agterm_core::ghostty_config::Font::default()
    };
    let fonts = Fonts::resolve(&missing, &|family| installed.iter().any(|name| name == family));
    assert_eq!(fonts.regular.family.as_ref(), DEFAULT_FAMILY);
    // the bold face is a face of its own, not the regular one emboldened
    let regular = text.resolve_font(&fonts.regular);
    let bold = text.resolve_font(&fonts.bold);
    assert_ne!(regular, bold);
}

#[test]
fn jetbrains_mono_measures_as_ghostty_measures_it() {
    let cx = embedded_only();
    let text = cx.text_system().clone();
    let id = text.resolve_font(&gpui_kit::font(DEFAULT_FAMILY));
    // upem 1000: advance 600, ascent 1020, descent -300, no line gap; post underline -155, 50 thick. At 12 points
    // (16 px): 9.6 x 21.12, the baseline 4.86 up, the underline 2.48 below it, 0.8 thick
    let cell: CellMetrics = metrics::cell_metrics(&text, id, 12.0, 1.0);
    assert_eq!((cell.cell_width, cell.cell_height, cell.cell_baseline), (10, 21, 5));
    assert_eq!(
        (cell.underline_position, cell.underline_thickness, cell.box_thickness),
        (18, 1, 1)
    );
    let face = metrics::measure(&text, id, px(16.0));
    assert!(face.line_gap.abs() < f64::EPSILON, "no line gap");
    assert!(
        face.underline_position
            .is_some_and(|position| (position + 2.48).abs() < 1e-3)
    );
    // OS/2 yStrikeoutPosition 320, yStrikeoutSize 50, read from the font as Ghostty's `FreeType` face reads them
    // (GPUI patch 0004), not estimated: 5.12 up, 0.8 thick
    assert!(
        face.strikethrough_position
            .is_some_and(|position| (position - 5.12).abs() < 1e-3),
        "{face:?}"
    );
    assert!(
        face.strikethrough_thickness
            .is_some_and(|thickness| (thickness - 0.8).abs() < 1e-3)
    );
    assert_eq!((cell.strikethrough_position, cell.strikethrough_thickness), (11, 1));
    // at 2x the box lines thicken with the underline: 1.6 rounds up to 2
    let hidpi = metrics::cell_metrics(&text, id, 12.0, 2.0);
    assert_eq!((hidpi.cell_width, hidpi.cell_height, hidpi.box_thickness), (19, 42, 2));
}

#[test]
fn the_lock_ghostty_draws_at_a_password_prompt_comes_from_the_symbols_fallback() {
    let mut cx = embedded_only();
    let installed = cx.text_system().all_font_names();
    let fonts = Fonts::resolve(&agterm_core::ghostty_config::Font::default(), &|family| {
        installed.iter().any(|name| name == family)
    });
    let cx = cx.add_empty_window();
    cx.update(|window, _| {
        let text = window.text_system();
        let run = TextRun {
            len: "\u{f023}".len(),
            font: fonts.regular.clone(),
            color: gpui_kit::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = text.layout_line("\u{f023}", px(16.0), &[run], None);
        let shaped = line.runs.first().expect("the lock is shaped");
        // the symbols font is a fallback only: GPUI takes no face without an 'm' as a primary one
        assert_ne!(
            shaped.font_id,
            text.resolve_font(&fonts.regular),
            "JetBrains Mono has no lock"
        );
        let glyph = shaped.glyphs.first().expect("one glyph");
        assert_ne!(glyph.id.0, 0, "not the missing-glyph box");
    });
}
