//! The title bar: hover texts and chord labels, the row per toolbar mode and decoration, the window controls per
//! layout, the traced glyphs (the GTK build's glyph tests), the glyph assets, and the bar over a core in GPUI's test
//! window following the toolbar mode, the toggles and the covers.

use agterm_core::model::WindowLibrary;
use agterm_core::{CoreConfig, CoreState};
use agterm_proto::AppIdentity;
use gpui_kit::{TestAppContext, Tiling, px, size};

use super::*;

#[test]
fn hover_texts_name_the_chord_when_bound() {
    assert_eq!(hint("Dashboard", Some("Shift+Ctrl+M")), "Dashboard (Shift+Ctrl+M)");
    assert_eq!(hint("Dashboard", None), "Dashboard");
}

#[test]
fn only_a_held_press_moves_the_window() {
    let mut pressed = true;
    assert!(starts_move(&mut pressed, Some(MouseButton::Left)));
    assert!(!pressed, "the move takes the press");
    // a press whose release landed past the bar: the next hover ends it rather than moving the window
    let mut pressed = true;
    assert!(!starts_move(&mut pressed, None));
    assert!(!pressed);
    assert!(!starts_move(&mut pressed, Some(MouseButton::Left)), "no press, no move");
}

#[test]
fn chords_read_as_gtk_labels_them() {
    let chord = |mods, key: &str| Chord::new(mods, key);
    assert_eq!(
        chord_label(&chord(Modifiers::CONTROL | Modifiers::SHIFT, "m")),
        "Shift+Ctrl+M"
    );
    assert_eq!(chord_label(&chord(Modifiers::CONTROL, "tab")), "Ctrl+Tab");
    assert_eq!(
        chord_label(&chord(Modifiers::COMMAND | Modifiers::OPTION, "f5")),
        "Alt+Super+F5"
    );
    assert_eq!(chord_label(&chord(Modifiers::CONTROL, "=")), "Ctrl+=");
    assert_eq!(chord_label(&chord(Modifiers::empty(), "delete")), "BackSpace");
}

#[test]
fn the_row_follows_the_mode_and_the_decorations() {
    assert_eq!(height(ToolbarMode::Normal, false), NORMAL_HEIGHT);
    assert_eq!(height(ToolbarMode::Compact, true), COMPACT_HEIGHT);
    assert_eq!(
        height(ToolbarMode::Hidden, true),
        COMPACT_HEIGHT,
        "the minimal bar carries the controls"
    );
    assert_eq!(
        height(ToolbarMode::Hidden, false),
        px(0.),
        "the window manager's bar is enough"
    );
}

#[test]
fn the_window_controls_follow_the_layout_and_what_the_window_manager_supports() {
    let client = Decorations::Client {
        tiling: Tiling::default(),
    };
    let all = WindowControls::default();
    let standard = WindowButtonLayout::linux_default();
    assert_eq!(
        controls(client, all, standard, false),
        [WindowButton::Minimize, WindowButton::Maximize, WindowButton::Close]
    );
    assert!(controls(client, all, standard, true).is_empty());
    assert!(
        controls(Decorations::Server, all, standard, false).is_empty(),
        "the window manager draws them"
    );
    let tiling_wm = WindowControls {
        minimize: false,
        maximize: false,
        ..WindowControls::default()
    };
    assert_eq!(controls(client, tiling_wm, standard, false), [WindowButton::Close]);
    let left = WindowButtonLayout::parse("close,minimize:").unwrap();
    assert_eq!(
        controls(client, all, left, true),
        [WindowButton::Close, WindowButton::Minimize]
    );
    assert!(controls(client, all, left, false).is_empty());
}

/// The filled area of `glyph` left and right of the slot's middle, inside the frame.
fn fill_by_half(glyph: SplitGlyph) -> (f32, f32) {
    let (mut left, mut right) = (0.0, 0.0);
    for stroke in glyph_strokes(PaneGlyph::Split(glyph)) {
        // the frame and the divider line carry no pane
        if !stroke.fill || stroke.width <= 1.0 || stroke.height <= 1.0 {
            continue;
        }
        let area = stroke.width * stroke.height;
        if stroke.x + stroke.width <= 8.0 {
            left += area;
        } else if stroke.x >= 8.0 {
            right += area;
        }
    }
    (left, right)
}

#[test]
fn a_hidden_split_fills_only_the_pane_on_screen() {
    let (left, right) = fill_by_half(SplitGlyph::Left);
    assert!(left > 0.0 && right == 0.0, "{left} vs {right}");
    let (left, right) = fill_by_half(SplitGlyph::Right);
    assert!(right > 0.0 && left == 0.0, "{left} vs {right}");
    let (left, right) = fill_by_half(SplitGlyph::Both);
    assert!(left > 0.0 && right > 0.0);
    assert_eq!(fill_by_half(SplitGlyph::None), (0.0, 0.0), "an empty outline");
    // stacked: the halves are on top of each other
    let top: Vec<_> = glyph_strokes(PaneGlyph::Split(SplitGlyph::Top))
        .into_iter()
        .filter(|stroke| stroke.fill && stroke.height > 1.0)
        .collect();
    assert_eq!(top.len(), 1);
    assert!(top[0].y + top[0].height <= 8.0, "{top:?}");
}

#[test]
fn the_scratch_glyph_fills_while_it_shows() {
    let filled = |active| {
        glyph_strokes(PaneGlyph::Scratch(active))
            .iter()
            .filter(|stroke| stroke.fill)
            .count()
    };
    assert_eq!(filled(true), 1);
    assert_eq!(filled(false), 0);
}

#[test]
fn a_long_host_keeps_both_ends() {
    assert_eq!(middle_ellipsis("user@host", 30), "user@host");
    let cut = middle_ellipsis("deploy@build-runner-0042.eu-central.example.com", 30);
    assert_eq!(cut.chars().count(), 30);
    assert!(
        cut.starts_with("deploy@build-ru") && cut.ends_with("example.com"),
        "{cut}"
    );
    assert!(cut.contains('…'));
}

#[test]
fn every_glyph_the_bar_draws_is_served() {
    for path in glyph_paths() {
        assert!(serves(&path), "{path}");
    }
    // gpui-kit's own component icons stay reachable behind the bar's
    assert!(serves(&IconName::ChevronDown.path()));
}

fn core(settings: agterm_core::persist::AppSettings) -> (Core, Uuid) {
    let library = WindowLibrary::seeded("/home/u");
    let window = library.active_window_id().unwrap();
    let core = Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("0.1.0", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library,
            settings,
            persistence: None,
        },
    );
    (core, window)
}

fn colors() -> ChromeColors {
    chrome::colors(
        &crate::terminal::TerminalConfig::default(),
        &agterm_core::persist::AppSettings::default(),
    )
}

/// The bar of `window` in a fresh test window, synced once over `core` under `cover`.
fn synced(cx: &mut TestAppContext, core: &Core, window: Uuid, cover: Cover) -> Bar {
    // the app menu's button is gpui-kit's, which reads its theme
    cx.update(gpui_kit::init);
    let (bar, cx) = cx.add_window_view(move |_, _| Titlebar::new(window, crate::state::detached_poster()));
    cx.simulate_resize(size(px(900.), px(600.)));
    bar.update(cx, |bar, cx| bar.sync(core, cover, false, colors(), cx));
    cx.run_until_parked();
    bar.read_with(cx, |bar, _| bar.bar().cloned().expect("synced"))
}

fn normal_mode() -> agterm_core::persist::AppSettings {
    agterm_core::persist::AppSettings {
        toolbar_mode: Some("normal".into()),
        attention_button_enabled: Some(true),
        ..Default::default()
    }
}

#[gpui_kit::test]
fn the_normal_bar_shows_both_lines_the_toggle_at_the_sidebars_edge_and_the_bell(cx: &mut TestAppContext) {
    let (core, window) = core(normal_mode());
    let shown = synced(cx, &core, window, Cover::None);
    assert_eq!(shown.model.mode, ToolbarMode::Normal);
    assert_eq!(shown.model.label.title, "u");
    assert_eq!(shown.model.label.subtitle, "/home/u", "normal mode's second line");
    assert!(shown.model.sidebar_toggle);
    let width = core.library().store(window).unwrap().sidebar().width;
    #[expect(clippy::cast_possible_truncation, reason = "a sidebar width fits an f32")]
    let width = width as f32;
    assert_eq!(shown.sidebar, Some(width), "the toggle sits at the sidebar's edge");
    assert!(
        shown.model.attention.is_some_and(|bell| !bell.enabled),
        "nothing needs attention"
    );
    assert_eq!(shown.hints.attention, "No sessions need attention (Shift+Ctrl+I)");
    assert!(shown.model.custom_commands.is_none(), "hidden by default");
    assert!(shown.model.dividers.after_jump && shown.model.dividers.after_panes);
}

#[gpui_kit::test]
fn a_cover_strips_the_bar_and_the_sidebar_steps_aside(cx: &mut TestAppContext) {
    let (core, window) = core(normal_mode());
    let zoomed = synced(cx, &core, window, Cover::Zoom);
    assert_eq!(zoomed.cover, Cover::Zoom);
    assert_eq!(zoomed.sidebar, None);
    let dashboard = synced(cx, &core, window, Cover::Dashboard("Dashboard".into()));
    assert_eq!(dashboard.cover, Cover::Dashboard("Dashboard".into()));
    assert_eq!(dashboard.sidebar, None);
}

#[gpui_kit::test]
fn the_hidden_and_compact_modes_and_the_interface_toggles(cx: &mut TestAppContext) {
    let (mut core, window) = core(normal_mode());
    core.update_settings(|settings| {
        settings.toolbar_mode = Some("hidden".into());
        settings.hidden_interface_elements = Some(vec!["recentSessions".into()]);
    });
    let hidden = synced(cx, &core, window, Cover::None);
    assert_eq!(hidden.model.mode, ToolbarMode::Hidden);
    assert_eq!(hidden.model.label, TitlebarComposition::default(), "no title");
    core.update_settings(|settings| settings.toolbar_mode = Some("compact".into()));
    let compact = synced(cx, &core, window, Cover::None);
    assert!(compact.model.recent.is_none());
    assert!(!compact.model.dividers.after_jump, "one jump button left");
    assert!(compact.model.label.subtitle.is_empty(), "compact: one line");
}

#[gpui_kit::test]
fn an_unchanged_sync_leaves_the_bar_alone(cx: &mut TestAppContext) {
    let (core, window) = core(agterm_core::persist::AppSettings::default());
    let bar = cx.new(|_| Titlebar::new(window, crate::state::detached_poster()));
    let notified = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = std::rc::Rc::clone(&notified);
    let _watch = cx.update(|cx| cx.observe(&bar, move |_, _| counter.set(counter.get() + 1)));
    bar.update(cx, |bar, cx| bar.sync(&core, Cover::None, false, colors(), cx));
    bar.update(cx, |bar, cx| bar.sync(&core, Cover::None, false, colors(), cx));
    cx.run_until_parked();
    assert_eq!(notified.get(), 1);
    let mut lighter = colors();
    lighter.background = Rgb::WHITE;
    bar.update(cx, |bar, cx| bar.sync(&core, Cover::None, false, lighter, cx));
    cx.run_until_parked();
    assert_eq!(notified.get(), 2, "a chrome change repaints the bar");
}
