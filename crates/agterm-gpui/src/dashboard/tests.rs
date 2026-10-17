//! The dashboard's grid shape, captions, keys, status chip and its pulse, and the font report rules (ported from the
//! GTK build's `dashboard.rs` and `surface.rs` tests).

use agterm_core::model::{AgentIndicator, AgentStatus, NewSession, SessionSeed, SplitLayout};
use gpui_kit::{Keystroke, Modifiers};

use super::*;
use crate::terminal::font_report;

fn key(name: &str, modifiers: Modifiers) -> Keystroke {
    Keystroke {
        modifiers,
        key: name.to_owned(),
        key_char: None,
    }
}

#[test]
fn the_rows_fill_row_major_and_pad_a_ragged_last_row() {
    assert_eq!(rows(1), vec![vec![Some(0)]]);
    assert_eq!(rows(3), vec![vec![Some(0), Some(1)], vec![Some(2), None]]);
    assert_eq!(
        rows(5),
        vec![vec![Some(0), Some(1), Some(2)], vec![Some(3), Some(4), None]]
    );
    assert_eq!(
        rows(12).iter().flatten().flatten().count(),
        agterm_core::dashboard::MAX_CELLS,
        "capped"
    );
    assert!(rows(0).is_empty());
}

#[test]
fn the_pane_marker_follows_the_split_axis() {
    let mut session = Session::new(SessionSeed::default());
    let id = session.id();
    assert_eq!(pane_marker(DashboardMember::primary(id), &session), "");
    session.split = SplitLayout {
        exists: true,
        ..SplitLayout::default()
    };
    assert_eq!(pane_marker(DashboardMember::primary(id), &session), " ◀");
    assert_eq!(pane_marker(DashboardMember::split(id), &session), " ▶");
    session.split.axis = SplitAxis::TopBottom;
    assert_eq!(pane_marker(DashboardMember::primary(id), &session), " ▲");
    assert_eq!(pane_marker(DashboardMember::split(id), &session), " ▼");
}

#[test]
fn the_grid_reads_bare_arrows_enter_and_escape() {
    let none = Modifiers::default();
    assert_eq!(grid_key(&key("left", none)), Some(GridKey::Move(Direction::Left)));
    assert_eq!(grid_key(&key("down", none)), Some(GridKey::Move(Direction::Down)));
    assert_eq!(grid_key(&key("enter", none)), Some(GridKey::Enter));
    assert_eq!(grid_key(&key("kp_enter", none)), Some(GridKey::Enter));
    assert_eq!(grid_key(&key("escape", none)), Some(GridKey::Close));
    assert_eq!(grid_key(&key("a", none)), None);
    assert_eq!(
        grid_key(&key("left", Modifiers::control())),
        None,
        "a chord is no grid key"
    );
}

#[test]
fn the_model_names_each_cell_marks_the_highlight_and_carries_the_status() {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let id = store.add_session(workspace, NewSession::at_cwd("/srv/app")).unwrap();
    store.set_split_visibility(id, true, None);
    store.session_mut(id).unwrap().agent_indicator = AgentIndicator {
        status: AgentStatus::Blocked,
        blink: true,
        ..AgentIndicator::default()
    };
    let model = GridModel {
        members: vec![DashboardMember::primary(id), DashboardMember::split(id)],
        highlighted: Some(DashboardMember::split(id)),
        font_size: None,
    };
    let cells = model.cells(Some(&store), &AppSettings::default());
    assert_eq!(cells[0].caption, "app ◀");
    assert_eq!(cells[1].caption, "app ▶");
    assert_eq!(
        cells.iter().map(|cell| cell.highlighted).collect::<Vec<_>>(),
        [false, true]
    );
    let status = cells[0].status.as_ref().expect("a blocked chip");
    assert_eq!(status.status, AgentStatus::Blocked);
    assert!(status.blink);
    assert_eq!(model.tokens(Some(&store)), [None, None], "no terminals yet");
    assert!(cells.iter().all(|cell| cell.cover.is_none()));
}

#[test]
fn the_pulse_rises_to_its_peak_and_back() {
    assert!(pulse_at(0.0).abs() < 1e-6);
    assert!((pulse_at(0.5) - WASH_PEAK).abs() < 1e-6);
    assert!(pulse_at(1.0).abs() < 1e-6);
    assert!(pulse_at(0.25) > 0.0 && pulse_at(0.25) < WASH_PEAK);
}

/// Upstream's `dashboardPillColor` / `dashboardPillTextColor`: an idle chip is the theme's selection pill (the selected
/// sidebar row's colors), the foreground at 0.22 over the backdrop without one; a status chip is the status color.
#[test]
fn an_idle_chip_takes_the_themes_selection_colors_else_a_foreground_wash() {
    let rgb = |r, g, b| agterm_core::chrome::Rgb::new(r, g, b);
    let mut cell = ChromeCell {
        member: DashboardMember::primary(uuid::Uuid::nil()),
        caption: "app".to_owned(),
        highlighted: false,
        cover: None,
        status: None,
    };
    let themed = Look {
        background: rgb(0x10, 0x10, 0x10),
        foreground: rgb(0xee, 0xee, 0xee),
        selection: Some((rgb(0x44, 0x47, 0x5a), rgb(0xf8, 0xf8, 0xf2))),
    };
    let (fill, text, wash) = chip_colors(&cell, themed, true);
    assert_eq!(fill, color(rgb(0x44, 0x47, 0x5a), 1.0), "the selection background");
    assert_eq!(
        text,
        color(rgb(0xf8, 0xf8, 0xf2), UNSELECTED_CAPTION),
        "the selection text, muted"
    );
    assert!(wash.is_none());
    cell.highlighted = true;
    let (_, text, _) = chip_colors(&cell, themed, true);
    assert_eq!(text, color(rgb(0xf8, 0xf8, 0xf2), 1.0), "full on the highlighted cell");
    let plain = Look {
        selection: None,
        ..themed
    };
    let (fill, text, _) = chip_colors(&cell, plain, true);
    assert_eq!(fill, color(rgb(0xee, 0xee, 0xee), 0.22), "the foreground wash");
    assert_eq!(text, color(rgb(0xee, 0xee, 0xee), 1.0));
    cell.status = Some(crate::status_glyph::GlyphLook {
        status: AgentStatus::Blocked,
        shape: agterm_core::model::StatusShape::Circle,
        rgb: (1.0, 0.584, 0.0),
        blink: true,
        tooltip: String::new(),
    });
    let (fill, text, wash) = chip_colors(&cell, themed, true);
    assert_eq!(
        fill,
        crate::status_glyph::hsla((1.0, 0.584, 0.0)),
        "the status color, not the selection"
    );
    assert_eq!(text, gpui_kit::black(), "contrasting text");
    assert_eq!(wash, Some(gpui_kit::white()), "a blinking chip pulses");
    assert!(
        chip_colors(&cell, themed, false).2.is_none(),
        "not while animations are off"
    );
}

#[test]
fn contrast_picks_dark_text_over_a_light_fill() {
    assert!(luminance((1.0, 0.584, 0.0)) > 0.5, "orange takes black text");
    assert!(luminance((0.1, 0.1, 0.3)) < 0.5, "a dark fill takes white text");
}

#[test]
fn an_override_reports_nothing_and_its_revert_is_dropped_once() {
    // while overridden, nothing is the pane's
    assert_eq!(font_report(Some(9.0), None, 9.0), (false, None));
    // the revert to the pending size is dropped (within half a point), and the next report is the pane's again
    assert_eq!(font_report(None, Some(13.0), 13.0), (false, None));
    assert_eq!(font_report(None, Some(13.0), 13.4), (false, None));
    assert_eq!(font_report(None, None, 14.0), (true, None));
    // a report that is not the revert is the pane's, and ends the wait for it
    assert_eq!(font_report(None, Some(13.0), 16.0), (true, None));
}
